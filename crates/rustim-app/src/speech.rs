//! Self-voicing: read the live region aloud without a screen reader.
//!
//! The accessibility layer speaks to whoever has a screen reader attached. On a
//! machine without one — which is most of them, and this one — nothing is said
//! at all. `--speak` closes that gap with the platform's own speech synthesiser:
//! text goes in on standard input, sound comes out of the desktop's audio.
//!
//! # Why this lives in the app and not the kernel
//!
//! The six wasm kernel crates may not spawn a process or do I/O (`CLAUDE.md`).
//! A speech backend is both, so it is here beside `motion`'s `gsettings` probe —
//! the one other place in the workspace that runs a program to answer a platform
//! question.
//!
//! # The text never goes on the command line
//!
//! A narration is arbitrary user-visible prose: spaces, quotes, newlines, CJK.
//! Passing it as an argument would need quoting per platform, would truncate on
//! some of them, and would leave the sentence visible in every process listing.
//! Every backend this module knows reads standard input, so the sentence is
//! written to a pipe and argument vectors stay constant. [`command_for`] is a
//! pure function precisely so that this property can be asserted for all three
//! platforms on any one of them.
//!
//! # Choosing a voice
//!
//! The backend's **default voice** is whatever the distribution configured, and
//! it is often an English voice. That is not merely an accent: `espeak-ng` with
//! an English voice reads every Han character as the words "Chinese letter", so
//! `场景 pythagorean，4 个对象` is heard as a burst of "Chinese letter …
//! pythagorean for Chinese letter Chinese letter Chinese letter Chinese letter"
//! while the caption shows the sentence. It sounds like the engine is repeating
//! itself; it is the voice saying one word per character.
//!
//! `--voice NAME` forwards the host's choice to whichever backend was found:
//! `-v NAME` as one argument for `espeak-ng`/`espeak` and macOS's `say`, and
//! `SpeechSynthesizer.SelectVoice` on Windows. The Windows name travels in an
//! **environment variable**, [`VOICE_ENV`], rather than being spliced into the
//! PowerShell one-liner — a voice name is host input, and the command string is
//! not the place to quote it. The module never picks a voice on its own: asking
//! whether a Mandarin voice exists means knowing each backend's listing syntax
//! (`espeak-ng --voices=zh`), and hard-coding `-v cmn` would break every machine
//! that lacks it. `piper` (a neural synthesiser needing downloaded model files)
//! is named in the design document as a future backend and is not implemented
//! here.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

/// The platforms this module knows how to speak on.
///
/// An enum rather than `cfg` alone because the command construction is a *pure
/// function of the platform*, and a test on Linux can then check what macOS and
/// Windows would run. The binary selects its own with [`Platform::current`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    /// The platform this binary was compiled for, or `None` elsewhere.
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Self::MacOs)
        } else if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else {
            None
        }
    }

    /// The programs to try, best first.
    ///
    /// `espeak-ng` is the maintained fork of `espeak` and is what the packaging
    /// in this repository's target distributions installs; the older name is a
    /// fallback because the two command lines are compatible. `say` ships with
    /// macOS. The Windows backend is Windows PowerShell 5.1, whose
    /// `System.Speech` assembly is in the .NET Framework — `pwsh` does not
    /// reliably carry it, so `powershell` is the program named.
    pub fn programs(self) -> &'static [&'static str] {
        match self {
            Self::Linux => &["espeak-ng", "espeak"],
            Self::MacOs => &["say"],
            Self::Windows => &["powershell"],
        }
    }
}

/// The environment variable that carries a Windows voice name.
///
/// Public so the tests and the documentation name the same thing. It is set on
/// the child process only; the module never touches the parent's environment.
pub const VOICE_ENV: &str = "RUSTIM_VOICE";

/// How to invoke one utterance on one platform.
///
/// The program is a name here, not a resolved path: [`find`] does the resolution
/// and [`ProcessVoice`] spawns the path it found. Keeping the name in the spec is
/// what makes the pure test platform-independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Variables to set for the child, as `(name, value)`.
    pub env: Vec<(String, String)>,
    /// What to write to the child's standard input before closing it.
    pub stdin: String,
}

/// The command for one line of speech, optionally in a named voice.
///
/// Pure, so the same three platforms can be checked from any one of them. All
/// three read the text from **standard input**: `espeak-ng --stdin`, `say` with
/// no file arguments (it reads stdin when there are none), and a PowerShell one-
/// liner that reads `[Console]::In` to its end and hands it to the synthesiser.
///
/// `voice` is forwarded verbatim; `None` leaves the backend's default alone. On
/// Windows the name goes through [`VOICE_ENV`] so it is never part of the
/// `-Command` string.
pub fn command_for(
    platform: Platform,
    program: &str,
    voice: Option<&str>,
    text: &str,
) -> CommandSpec {
    let mut args: Vec<String> = Vec::new();
    let mut env: Vec<(String, String)> = Vec::new();
    match platform {
        Platform::Linux => {
            if let Some(voice) = voice {
                args.push("-v".to_owned());
                args.push(voice.to_owned());
            }
            args.push("--stdin".to_owned());
        }
        Platform::MacOs => {
            if let Some(voice) = voice {
                args.push("-v".to_owned());
                args.push(voice.to_owned());
            }
        }
        Platform::Windows => {
            args.extend(
                [
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Add-Type -AssemblyName System.Speech; $s = New-Object \
                     System.Speech.Synthesis.SpeechSynthesizer; if ($env:RUSTIM_VOICE) \
                     { $s.SelectVoice($env:RUSTIM_VOICE) }; $s.Speak([Console]::In.ReadToEnd())",
                ]
                .into_iter()
                .map(str::to_owned),
            );
            if let Some(voice) = voice {
                env.push((VOICE_ENV.to_owned(), voice.to_owned()));
            }
        }
    }
    CommandSpec {
        program: program.to_owned(),
        args,
        env,
        stdin: text.to_owned(),
    }
}

/// The first program in `programs` that exists on `path`.
///
/// `PATH` is passed in rather than read so that a test can hand it a directory
/// it owns and leave the process environment alone — mutating `PATH` in a test
/// would race every other test in the binary.
pub fn find_any_in(path: &OsStr, programs: &[&'static str]) -> Option<(PathBuf, &'static str)> {
    programs
        .iter()
        .find_map(|program| find_in(path, program).map(|path| (path, *program)))
}

/// Resolve one program against `path`.
///
/// On Unix a candidate must be a regular file with at least one execute bit set;
/// on Windows the usual `.exe` suffix is tried as well. The suffix list is not
/// `PATHEXT` — a speech backend is a native executable in every case this module
/// supports, and reproducing the full PATHEXT search is more machinery than the
/// question needs.
pub fn find_in(path: &OsStr, program: &str) -> Option<PathBuf> {
    let mut candidates = vec![program.to_owned()];
    if cfg!(windows) && !program.to_ascii_lowercase().ends_with(".exe") {
        candidates.push(format!("{program}.exe"));
    }
    std::env::split_paths(path).find_map(|dir| {
        candidates
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_runnable(candidate))
    })
}

/// [`find_in`] against the process's own `PATH`.
pub fn find(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| find_in(&path, program))
}

fn is_runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Something that can say a line, abstracted so the queue is testable.
pub trait Voice: Send {
    fn speak(&mut self, text: &str);
}

/// A voice implemented by a child process.
///
/// One process per utterance, because that is the interface all three backends
/// offer: `espeak-ng --stdin` and `say` both speak a stream and exit, and
/// PowerShell's `Speak` returns when the sentence ends. A long-lived process
/// would need a line protocol none of them has.
///
/// The stdin pipe must be **closed before waiting**. `ReadToEnd`, `--stdin` and
/// `say` all read until end of input, so a parent that kept the write end open
/// and called `wait` would deadlock against its own child. Dropping the writer
/// is the close.
pub struct ProcessVoice {
    program: PathBuf,
    platform: Platform,
    voice: Option<String>,
    disabled: bool,
}

impl ProcessVoice {
    /// `voice` is the backend's own voice name (`cmn`, `en-us`, `Tingting`, …);
    /// `None` keeps the distribution's default. The name is not validated here
    /// — the backend knows its own vocabulary, and a wrong name is its refusal.
    pub fn new(program: PathBuf, platform: Platform, voice: Option<String>) -> Self {
        Self {
            program,
            platform,
            voice,
            disabled: false,
        }
    }
}

impl Voice for ProcessVoice {
    fn speak(&mut self, text: &str) {
        if self.disabled {
            return;
        }
        // The program field of the spec is a name for the pure test; the spawn
        // below uses the resolved path instead.
        let spec = command_for(self.platform, "", self.voice.as_deref(), text);
        let spawned = Command::new(&self.program)
            .args(&spec.args)
            .envs(spec.env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                // Once per run, not once per sentence: a missing backend would
                // otherwise print on every narration line.
                eprintln!(
                    "speech: {} could not start ({e}); self-voicing is off for the rest of the run",
                    self.program.display()
                );
                self.disabled = true;
                return;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            // A broken pipe means the synthesizer exited without reading; that
            // is its answer, not a reason to take the speaker thread down.
            let _ = stdin.write_all(spec.stdin.as_bytes());
            let _ = stdin.write_all(b"\n");
            // `stdin` is dropped here, closing the child's input.
        }
        let _ = child.wait();
    }
}

/// A queue in front of a [`Voice`], so the frame loop never blocks on speech.
///
/// Speech takes seconds and a frame takes milliseconds. The thread owns the
/// voice and says lines in order; the event loop only sends. Dropping the
/// speaker closes the queue, and the thread finishes whatever it is saying —
/// deliberately **without** joining, because a window that is closing should not
/// wait for the last sentence, and a `--frames` run that exits mid-utterance is
/// allowed to cut it off.
pub struct Speaker {
    tx: Option<Sender<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Speaker {
    pub fn spawn<V: Voice + 'static>(mut voice: V) -> Self {
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let handle = thread::spawn(move || {
            while let Ok(Some(text)) = rx.recv() {
                voice.speak(&text);
            }
        });
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }

    /// Queue a line. A closed queue (the thread died) drops it silently.
    pub fn say(&self, text: &str) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Some(text.to_owned()));
        }
    }

    /// Say everything queued and wait for the queue to drain.
    ///
    /// Tests only: the run paths deliberately do not wait, and this method is the
    /// only way to make "two lines were spoken in order" a deterministic
    /// assertion rather than a sleep.
    pub fn finish(mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(None);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        // Closing the sender is enough: the thread's `recv` returns `Err` once
        // the current utterance ends. No join — see the type's docs.
        let _ = self.tx.take();
    }
}

/// Report the failure to find a backend in the one line the user can act on.
pub fn missing_report(platform: Platform) -> String {
    let names = platform
        .programs()
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(" or ");
    format!("speech: no {names} on PATH; --captions still works")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const SAID: &str = "两条直角边是 3 和 4";

    #[test]
    fn the_text_is_on_stdin_and_never_in_the_arguments() {
        for platform in [Platform::Linux, Platform::MacOs, Platform::Windows] {
            for voice in [None, Some("cmn")] {
                let spec = command_for(platform, "voice", voice, SAID);
                assert_eq!(spec.stdin, SAID, "{platform:?}");
                assert!(
                    spec.args.iter().all(|arg| !arg.contains(SAID)),
                    "{platform:?} put the sentence on the command line: {:?}",
                    spec.args
                );
                assert!(
                    spec.args.iter().all(|arg| !arg.contains("直角")),
                    "{platform:?} put part of the sentence on the command line"
                );
                assert!(
                    spec.env.iter().all(|(_, value)| !value.contains(SAID)),
                    "{platform:?} put the sentence in the environment: {:?}",
                    spec.env
                );
            }
        }
    }

    #[test]
    fn each_platform_runs_the_backend_it_has() {
        assert_eq!(
            command_for(Platform::Linux, "espeak-ng", None, SAID).args,
            ["--stdin"]
        );
        assert!(
            command_for(Platform::MacOs, "say", None, SAID)
                .args
                .is_empty()
        );
        let windows = command_for(Platform::Windows, "powershell", None, SAID);
        assert_eq!(&windows.args[..2], ["-NoProfile", "-NonInteractive"]);
        assert!(windows.args[3].contains("System.Speech"));
        assert!(windows.args[3].contains("[Console]::In.ReadToEnd()"));
    }

    /// The voice name is the host's, and each backend spells it its own way:
    /// an argument with no quoting to get wrong on the two Unix platforms, and
    /// an environment variable on Windows so it is never spliced into the
    /// `-Command` string.
    #[test]
    fn a_named_voice_reaches_the_backend_without_touching_the_text() {
        let linux = command_for(Platform::Linux, "espeak-ng", Some("cmn"), SAID);
        assert_eq!(linux.args, ["-v", "cmn", "--stdin"]);
        assert!(linux.env.is_empty());

        let macos = command_for(Platform::MacOs, "say", Some("Tingting"), SAID);
        assert_eq!(macos.args, ["-v", "Tingting"]);
        assert!(macos.env.is_empty());

        let windows = command_for(Platform::Windows, "powershell", Some("Tingting"), SAID);
        assert_eq!(windows.env, [(VOICE_ENV.to_owned(), "Tingting".to_owned())]);
        assert!(
            windows.args[3].contains("$env:RUSTIM_VOICE"),
            "the voice never reaches the child: {:?}",
            windows.args
        );
        assert!(
            !windows.args.iter().any(|arg| arg.contains("Tingting")),
            "the voice was spliced into the PowerShell command: {:?}",
            windows.args
        );
    }

    /// What the previous test proves about the spec, this proves about the
    /// spawn: `ProcessVoice` must use the spec it was configured with. Unix
    /// only, because the fake backend is a shell script.
    #[cfg(unix)]
    #[test]
    fn process_voice_passes_the_voice_and_the_text_on_stdin() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_dir("spawn");
        let program = dir.join("fake-voice");
        // `$0` keeps the shell script free of interpolated paths, so a temp
        // directory with a quote or a space in it cannot break the fixture.
        let args_out = dir.join("fake-voice.args");
        let stdin_out = dir.join("fake-voice.stdin");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\ncat > \"$0.stdin\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut voice = ProcessVoice::new(program.clone(), Platform::Linux, Some("cmn".to_owned()));
        voice.speak(SAID);

        let args = std::fs::read_to_string(&args_out).unwrap();
        assert_eq!(args, "-v\ncmn\n--stdin\n");
        let stdin = std::fs::read_to_string(&stdin_out).unwrap();
        assert_eq!(stdin, format!("{SAID}\n"));

        // The macOS spec through the same fake backend. `say` takes no `--stdin`
        // flag — it reads standard input when it is given no file — so the whole
        // difference between the two spellings is that one argv entry. Running it
        // here rather than on the machine that has `say` is the point: `Platform`
        // exists so a spec can be *constructed* everywhere, and this is the one
        // place a spawn is checked against it, so pinning it to `Platform::Linux`
        // left the `say` argv unwitnessed by any test.
        let mut voice = ProcessVoice::new(program, Platform::MacOs, Some("Tingting".to_owned()));
        voice.speak(SAID);

        let args = std::fs::read_to_string(&args_out).unwrap();
        assert_eq!(args, "-v\nTingting\n");
        let stdin = std::fs::read_to_string(&stdin_out).unwrap();
        assert_eq!(
            stdin,
            format!("{SAID}\n"),
            "`say` reads the sentence from stdin on both spellings"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_program_lists_are_in_preference_order() {
        assert_eq!(Platform::Linux.programs(), ["espeak-ng", "espeak"]);
        assert_eq!(Platform::MacOs.programs(), ["say"]);
        assert_eq!(Platform::Windows.programs(), ["powershell"]);
        let expected = if cfg!(target_os = "linux") {
            Some(Platform::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Platform::MacOs)
        } else if cfg!(target_os = "windows") {
            Some(Platform::Windows)
        } else {
            None
        };
        assert_eq!(Platform::current(), expected);
    }

    #[test]
    fn the_missing_backend_report_names_what_was_looked_for() {
        let report = missing_report(Platform::Linux);
        assert!(
            report.contains("espeak-ng") && report.contains("espeak"),
            "{report}"
        );
        assert!(report.contains("--captions"), "{report}");
    }

    fn unique_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustim-speech-{}-{tag}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_program_is_found_only_when_it_is_executable() {
        let dir = unique_dir("find");
        let name = if cfg!(windows) { "voice.exe" } else { "voice" };
        let program = dir.join(name);
        std::fs::write(&program, b"").unwrap();

        // Present but not executable: not a backend.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(find_in(dir.as_os_str(), "voice"), None);
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(find_in(dir.as_os_str(), "voice"), Some(program));

        // A directory of that name is not a program either.
        let sub = dir.join("dir");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(find_in(dir.as_os_str(), "dir"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_any_in_takes_the_first_candidate_present() {
        let dir = unique_dir("any");
        let name = if cfg!(windows) {
            "second.exe"
        } else {
            "second"
        };
        let program = dir.join(name);
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let found = find_any_in(dir.as_os_str(), &["first", "second"]);
        assert_eq!(found, Some((program, "second")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl Voice for Recorder {
        fn speak(&mut self, text: &str) {
            self.0.lock().unwrap().push(text.to_owned());
        }
    }

    #[test]
    fn the_speaker_says_lines_in_order_and_finishes_when_asked() {
        let recorder = Recorder::default();
        let speaker = Speaker::spawn(recorder.clone());
        speaker.say("第一句");
        speaker.say("第二句");
        speaker.finish();
        assert_eq!(*recorder.0.lock().unwrap(), ["第一句", "第二句"]);
    }

    /// Dropping without `finish` must not hang the caller — the thread is
    /// detached and the window can close mid-sentence.
    #[test]
    fn dropping_the_speaker_does_not_wait_for_speech() {
        let recorder = Recorder::default();
        let speaker = Speaker::spawn(recorder.clone());
        speaker.say("说完就退出");
        drop(speaker);
        // The worker owns a clone; whether it has run by now is a race, but the
        // drop itself returning is the property under test.
        assert!(recorder.0.lock().is_ok());
    }
}
