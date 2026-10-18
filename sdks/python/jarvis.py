#!/usr/bin/env python3
"""jarvis.py -- a minimal AI director for rustim.

rustim and the assistant are separate products.  This file is an *example
assistant*: it asks a language model for a lecture plan, checks the plan
against the real engine, and then drives the engine over the documented stdio
protocol.  It contains no engine logic, links no engine crate, and knows
nothing about rendering.

The model is any OpenAI-compatible chat-completions endpoint:

    RUSTIM_LLM_BASE    default https://api.deepseek.com
    RUSTIM_LLM_MODEL   default deepseek-flash
    RUSTIM_LLM_KEY     else DEEPSEEK_API_KEY, else OPENAI_API_KEY

Run it:

    python3 -I sdks/python/jarvis.py "用三步动画解释勾股定理" --out /tmp/lesson.mp4
    python3 -I sdks/python/jarvis.py --offline --out /tmp/lesson.mp4
    python3 -I sdks/python/jarvis.py --plan /tmp/plan.json --out /tmp/lesson.mp4

The plan format is the protocol's own `plan.load` document: a list of steps,
each with an optional `say`, optional `ops` (ordinary commands) and optional
`beats` (animation specs).  The engine validates every command on scratch state
through `validate` before a single frame is rendered; a refusal is sent back to
the model as a repair hint.

Narration is rewritten as `narrate` commands with an explicit `at`, computed
from the plan's beat durations.  That is deliberate: a file feed applies every
command at virtual time zero, so a `say` left inside the plan would be due
immediately and the whole lecture would speak at once.  The plan itself remains
the model's product; only the timing is the driver's job.

Captions and sound are on by default.  The engine draws the live region into
the bottom of every frame (`--captions`) and writes a `.srt`; it does not speak
during an export, so this host synthesises the same lines with espeak-ng and
muxes them into the mp4.  `--no-captions` and `--no-speak` turn each half off.
"""

import argparse
import array
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import wave

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rustim_client import Client, ProtocolError  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
STEP = 1.0 / 60.0
MAX_STEPS = 12
MAX_REPAIRS = 2
TAIL_SECONDS = 2.0
MAX_FRAMES = 7200

#: The voice espeak-ng reads Chinese prose with.  The backend's default is the
#: distribution's, and an English voice does not mis-pronounce Han characters —
#: it spells them out as "Chinese letter", once per character.
DEFAULT_VOICE = "cmn"

SYSTEM_PROMPT = """你是 rustim 的讲解导演。rustim 是一个数学动画引擎。你只输出一个 JSON
讲解计划，由外部程序交给引擎执行。

坐标：世界单位，画面高 8、宽约 14.2，原点在画面中心，y 向上。
颜色建议：蓝 [100,181,246]、黄 [255,213,79]、红 [239,83,80]、白 [255,255,255]。

计划结构（严格）：
{"label":"计划名","steps":[
  {"say":"这一句讲解","ops":[...命令...],"beats":[...动画...]}
]}
每一步的 say / ops / beats 都可省略，但不能三者全空。3 到 6 步。只输出 JSON，
不要 Markdown，不要解释。

可用的 ops 命令：
1. 创建图形：
   {"action":"add","type":"circle","params":{"radius":1.0,"position":[0,0],
    "color":[100,181,246],"label":"圆"}}
   其他 type 与 params：square / rectangle 用 width,height；line / arrow 用 width；
   polygon 用 points（[[x,y],...]，至少 3 个，世界坐标）；text 用 text,size；
   formula 用 text（Typst 数学，不含 $）；number_line 用 width,range:[min,max],step；
   axes 用 width,height,x_range,y_range,step；plot 用 points（已采样的世界坐标）。
   每个 add 必须带一个全计划唯一的 label；之后一律用 label 引用，不要用数字 id。
2. 布局：{"action":"next_to","target":"标签","reference":"标签",
   "direction":"left|right|up|down","buff":0.2}
   {"action":"arrange","targets":["a","b"],"axis":"row|column","buff":0.3}
   {"action":"fit","targets":["a"],"width":3.0,"height":2.0}
3. 教学模板：
   {"action":"explain.equation","formula":"a^2+b^2=c^2","caption":"勾股定理",
    "position":[0,2.0],"label":"方程"}
   {"action":"explain.geometry","vertices":[[0,0],[3,0],[0,2.4]],
    "side_labels":["a","c","b"],"right_angle":true,"position":[0,-1.0],"label":"三角形"}
   （side_labels 按边顺序：第 i 个标签属于 vertices[i] → vertices[i+1] 这条边；
    所以直角三角形的斜边是第 1 条边。）
   {"action":"explain.plot","coefficients":[0,0,0.5],"x_range":[-3,3],
    "y_range":[-1,4],"caption":"y = 0.5x²","label":"抛物线"}
   注意：模板的 label 用来称呼整个模板组；不要使用 "公式" 作为 label。
4. 修改与镜头：
   {"action":"set","target":"标签","property":{"name":"fill_opacity","value":0.3}}
   {"action":"camera","zoom":1.4,"duration":1.0}
   {"action":"remove","target":"标签"}
   {"action":"narrate","text":"一句旁白"}   （优先用 step 的 say，而不是这个）

可用的 beats（每条是一个动画，时长必须写，0.4 到 2.0 秒）：
{"type":"fade_in","target":"标签","duration":0.8}
{"type":"fade_out","target":"标签","duration":0.8}
{"type":"move_to","target":"标签","to":[x,y],"duration":1.0}
{"type":"rotate","target":"标签","angle":1.57,"duration":1.0}
{"type":"scale","target":"标签","factor":1.5,"duration":0.8}
{"type":"create","target":"标签","duration":1.0}
{"type":"transform","target":"标签A","into":"标签B","duration":1.0}
{"type":"animate","target":"标签","property":{"name":"opacity","value":0.2},
 "duration":1.0}
{"type":"group","animations":[...],"lag_ratio":0.0}
{"type":"succession","animations":[...]}
group 表示同时播放，succession 表示依次播放。只使用这些 type，不要用 query、
check、plan.*、batch、validate 之类的命令。

关于动画的两条硬规则：
1. 新建对象的 opacity 默认是 1，而 fade_in 是从对象当前的不透明度升到 1，所以对
   刚建好的对象直接 fade_in 没有任何视觉效果。要让对象淡入，先
   {"action":"set","target":"标签","property":{"name":"opacity","value":0}}；
   或者用 create，让带点的图形沿路径画出来。group 的淡入要求每个成员都是 0。
2. params.color 会同时设置描边色和填充色。画“带边框的浅色方块”时，先 set
   fill_opacity（如 0.35），再 set stroke_opacity（如 0.9）。

内容要求：先用 ops 把对象创建好（可以放在第一步），再用 beats 让它们出现或移动；
讲解用简短中文（每句不超过 30 字）；图形不要超出 x ∈ [-6.5, 6.5]、
y ∈ [-3.8, 3.8]；画面底部 0.9 个世界单位留给字幕条，图形下沿不要低于 y = -3.0。

讲勾股定理时必须画出三条边上的正方形（a²、b²、c²），只画一个三角形是不够的：
直角边 a、b 各是一个正方形，斜边 c 上的正方形由斜边向量旋转 90° 得到；三个正方形、
三角形、直角标记和标签最后放进一个 add group，整体淡入。

示例（五步讲勾股定理；直角边 1.4 和 1.05，斜边 1.75）：
{"label":"勾股定理","steps":[
 {"say":"先写下勾股定理。","ops":[
   {"action":"explain.equation","formula":"a^2 + b^2 = c^2","caption":"勾股定理",
    "caption_size":0.5,"position":[0,3.4],"label":"方程"},
   {"action":"set","target":"方程","property":{"name":"scale","value":0.6}}],
  "beats":[{"type":"scale","target":"方程","factor":1.6666666666666667,"duration":1.2}]},
 {"say":"三条边各画一个正方形。","ops":[
   {"action":"add","type":"polygon","params":{"label":"a方","color":[100,181,246],
    "points":[[-0.7,-1.55],[0.7,-1.55],[0.7,-2.95],[-0.7,-2.95]]}},
   {"action":"set","target":"a方","property":{"name":"fill_opacity","value":0.35}},
   {"action":"set","target":"a方","property":{"name":"stroke_opacity","value":0.9}},
   {"action":"set","target":"a方","property":{"name":"opacity","value":0}},
   {"action":"add","type":"polygon","params":{"label":"b方","color":[255,213,79],
    "points":[[-0.7,-1.55],[-0.7,-0.5],[-1.75,-0.5],[-1.75,-1.55]]}},
   {"action":"set","target":"b方","property":{"name":"fill_opacity","value":0.35}},
   {"action":"set","target":"b方","property":{"name":"stroke_opacity","value":0.9}},
   {"action":"set","target":"b方","property":{"name":"opacity","value":0}},
   {"action":"add","type":"polygon","params":{"label":"c方","color":[239,83,80],
    "points":[[0.7,-1.55],[-0.7,-0.5],[0.35,0.9],[1.75,-0.15]]}},
   {"action":"set","target":"c方","property":{"name":"fill_opacity","value":0.35}},
   {"action":"set","target":"c方","property":{"name":"stroke_opacity","value":0.9}},
   {"action":"set","target":"c方","property":{"name":"opacity","value":0}},
   {"action":"add","type":"polygon","params":{"label":"三角形","color":[255,255,255],
    "points":[[-0.7,-1.55],[0.7,-1.55],[-0.7,-0.5]]}},
   {"action":"set","target":"三角形","property":{"name":"fill_opacity","value":0.15}},
   {"action":"set","target":"三角形","property":{"name":"stroke_opacity","value":0.9}},
   {"action":"set","target":"三角形","property":{"name":"opacity","value":0}},
   {"action":"add","type":"polygon","params":{"label":"直角标记","color":[255,255,255],
    "points":[[-0.7,-1.55],[-0.55,-1.55],[-0.55,-1.4],[-0.7,-1.4]]}},
   {"action":"set","target":"直角标记","property":{"name":"opacity","value":0}},
   {"action":"add","type":"text","params":{"label":"a²","text":"a²","size":0.5,
    "position":[0,-2.25],"color":[255,255,255]}},
   {"action":"set","target":"a²","property":{"name":"opacity","value":0}},
   {"action":"add","type":"text","params":{"label":"b²","text":"b²","size":0.5,
    "position":[-1.225,-1.025],"color":[255,255,255]}},
   {"action":"set","target":"b²","property":{"name":"opacity","value":0}},
   {"action":"add","type":"text","params":{"label":"c²","text":"c²","size":0.5,
    "position":[0.525,-0.325],"color":[255,255,255]}},
   {"action":"set","target":"c²","property":{"name":"opacity","value":0}},
   {"action":"add","type":"group","params":{"label":"图形"},
    "children":["a方","b方","c方","三角形","直角标记","a²","b²","c²"]}],
  "beats":[{"type":"group","lag_ratio":0.08,"animations":[
    {"type":"fade_in","target":"a方","duration":0.9},
    {"type":"fade_in","target":"b方","duration":0.9},
    {"type":"fade_in","target":"c方","duration":0.9},
    {"type":"fade_in","target":"三角形","duration":0.9},
    {"type":"fade_in","target":"直角标记","duration":0.9},
    {"type":"fade_in","target":"a²","duration":0.9},
    {"type":"fade_in","target":"b²","duration":0.9},
    {"type":"fade_in","target":"c²","duration":0.9}]}]},
 {"say":"看斜边上的红色正方形。","beats":[
   {"type":"animate","target":"c方","property":{"name":"fill_opacity","value":0.75},
    "duration":1.4}]},
 {"say":"直角边的平方和等于它。","beats":[
   {"type":"animate","target":"c方","property":{"name":"fill_opacity","value":0.35},
    "duration":1.6}]},
 {"say":"这就是勾股定理。"}
]}
"""


class JarvisError(Exception):
    """A failure the user can act on: no key, no binary, a refused plan."""


def find_binary(explicit):
    if explicit:
        return explicit
    from_env = os.environ.get("RUSTIM_BIN")
    if from_env:
        return from_env
    for profile in ("release", "debug"):
        path = os.path.join(ROOT, "target", profile, "rustim-app")
        if os.path.exists(path):
            return path
    return os.path.join(ROOT, "target", "debug", "rustim-app")


def llm_settings():
    base = os.environ.get("RUSTIM_LLM_BASE", "https://api.deepseek.com").rstrip("/")
    model = os.environ.get("RUSTIM_LLM_MODEL", "deepseek-flash")
    key = (
        os.environ.get("RUSTIM_LLM_KEY")
        or os.environ.get("DEEPSEEK_API_KEY")
        or os.environ.get("OPENAI_API_KEY")
    )
    return base, model, key


def http_json(url, body, key, timeout=120):
    data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=data,
        headers={
            "Content-Type": "application/json",
            "Authorization": "Bearer " + key,
        },
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def ask_model(messages):
    base, model, key = llm_settings()
    if not key:
        raise JarvisError(
            "没有可用的模型密钥：设置 RUSTIM_LLM_KEY、DEEPSEEK_API_KEY 或 "
            "OPENAI_API_KEY，或者用 --offline 跑内置示例。"
        )
    url = base + "/chat/completions"
    body = {
        "model": model,
        "messages": messages,
        "temperature": 0.2,
        "response_format": {"type": "json_object"},
    }
    try:
        document = http_json(url, body, key)
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", "replace")[:500]
        # Some deployments reject `response_format`; ask the same question
        # without it rather than failing the whole run.
        if error.code == 400 and "json" in detail.lower():
            body.pop("response_format", None)
            try:
                document = http_json(url, body, key)
            except (urllib.error.URLError, urllib.error.HTTPError) as second:
                raise JarvisError("模型接口失败：%s" % second)
        else:
            raise JarvisError("模型接口返回 HTTP %d：%s" % (error.code, detail))
    except urllib.error.URLError as error:
        raise JarvisError("连不上模型接口 %s：%s" % (url, error))
    try:
        return document["choices"][0]["message"]["content"]
    except (KeyError, IndexError, TypeError):
        raise JarvisError("模型回复里没有 choices[0].message.content：%r" % document)


def extract_json(text):
    """Pull the first JSON object out of a model reply.

    Models still wrap JSON in a code fence or add a sentence in front of it
    even when asked not to.  This is not a parser: it finds the outermost
    braces and lets `json.loads` decide.
    """
    if not isinstance(text, str):
        raise ValueError("回复不是文本")
    stripped = text.strip()
    fence = re.search(r"```(?:json)?\s*(.*?)```", stripped, re.S)
    if fence:
        stripped = fence.group(1).strip()
    start = stripped.find("{")
    end = stripped.rfind("}")
    if start < 0 or end <= start:
        raise ValueError("回复里没有 JSON 对象")
    return json.loads(stripped[start : end + 1])


def normalize_plan(plan):
    """Accept either the plan itself or a `{"plan": ...}` wrapper."""
    if isinstance(plan, dict) and "steps" not in plan and isinstance(plan.get("plan"), dict):
        return plan["plan"]
    return plan


def local_problem(plan):
    """Structural checks the engine's `validate` cannot express as commands."""
    if not isinstance(plan, dict):
        return "顶层必须是 JSON 对象"
    steps = plan.get("steps")
    if not isinstance(steps, list) or not steps:
        return "steps 必须是非空数组"
    if len(steps) > MAX_STEPS:
        return "steps 有 %d 步，最多 %d 步" % (len(steps), MAX_STEPS)
    labels = []
    for index, step in enumerate(steps):
        if not isinstance(step, dict):
            return "steps[%d] 不是对象" % index
        if not (step.get("say") or step.get("ops") or step.get("beats")):
            return "steps[%d] 的 say / ops / beats 全空" % index
        ops = step.get("ops", [])
        if not isinstance(ops, list):
            return "steps[%d].ops 必须是数组" % index
        for member, op in enumerate(ops):
            if not isinstance(op, dict) or not isinstance(op.get("action"), str):
                return "steps[%d].ops[%d] 必须是含 action 的对象" % (index, member)
            action = op["action"]
            if action in ("query", "check", "hello", "batch", "validate") or action.startswith("plan."):
                return "steps[%d].ops[%d] 不能是 %s" % (index, member, action)
            label = None
            if action == "add":
                params = op.get("params")
                if isinstance(params, dict):
                    label = params.get("label")
            elif action.startswith("explain.") or action in (
                "point_at",
                "annotate",
                "highlight",
            ):
                label = op.get("label")
            if label:
                if label == "公式" and action == "explain.equation":
                    return "steps[%d].ops[%d] 的 label 不能是 \"公式\"（会和模板内部的名字冲突）" % (
                        index,
                        member,
                    )
                labels.append(label)
        beats = step.get("beats", [])
        if not isinstance(beats, list):
            return "steps[%d].beats 必须是数组" % index
        for member, beat in enumerate(beats):
            if not isinstance(beat, dict) or not isinstance(beat.get("type"), str):
                return "steps[%d].beats[%d] 必须是含 type 的对象" % (index, member)
    duplicates = sorted({label for label in labels if labels.count(label) > 1})
    if duplicates:
        return "这些 label 重复了：%s；每个对象必须有一个全计划唯一的 label" % ", ".join(
            duplicates
        )
    return None


def content_problem(plan, request):
    """The one content rule the engine cannot state: a theorem's picture.

    `validate` and `check` can say a scene is well formed and even legible;
    neither can say that an explanation of Pythagoras has to *show* the squares
    the theorem is about.  A plan that draws only the triangle is valid JSON,
    passes the linter, and teaches nothing, which is exactly the failure this
    check exists for.  It is deliberately narrow: it fires only when the
    request names the theorem, and asks for the three polygons that carry the
    picture rather than prescribing the whole scene.
    """
    asked = request.lower()
    if "勾股" not in request and "pythagor" not in asked:
        return None
    polygons = 0
    for step in plan.get("steps") or []:
        for op in step.get("ops") or []:
            # `type` is a field of the `add` command itself, beside `params` —
            # not one of the shape's parameters.
            if op.get("action") == "add" and op.get("type") == "polygon":
                polygons += 1
    if polygons >= 3:
        return None
    return (
        "勾股定理必须画出三条边上的正方形，只画三角形不够：用 add polygon 画 a²、b²、c² "
        "三个正方形（斜边上的正方形由斜边向量旋转 90° 得到），再把三角形、正方形、直角"
        "标记和标签放进一个 add group，整体淡入。"
    )


def beat_duration(spec):
    """The nominal length of one animation spec, mirroring the engine.

    An explicit `duration` wins.  A group or succession without one is laid out
    Manim-style: start[i] = start[i-1] + d[i-1] * lag, total = max(start + d).
    Every other kind defaults to Manim's DEFAULT_ANIMATION_RUN_TIME (1.0 s).
    """
    if not isinstance(spec, dict):
        return 1.0
    stated = spec.get("duration")
    if isinstance(stated, (int, float)) and not isinstance(stated, bool) and stated > 0:
        return float(stated)
    kind = spec.get("type")
    if kind in ("group", "succession"):
        members = spec.get("animations") or []
        if not isinstance(members, list) or not members:
            return 1.0
        durations = [beat_duration(member) for member in members]
        lag = spec.get("lag_ratio")
        if lag is None:
            lag = 0.0 if kind == "group" else 1.0
        try:
            lag = float(lag)
        except (TypeError, ValueError):
            lag = 0.0 if kind == "group" else 1.0
        total = 0.0
        start = 0.0
        for index, duration in enumerate(durations):
            if index > 0:
                start = start + durations[index - 1] * lag
            total = max(total, start + duration)
        return total
    return 1.0


def flatten(plan):
    """Turn a plan into the flat command log an export can replay.

    Returns `(commands, seconds)`.  `say` becomes `narrate` with an explicit
    `at`, and every beat becomes a `play`; the clock advances by each beat's
    nominal duration so the narration lines land on the beats they describe.
    """
    commands = []
    now = 0.0
    for step in plan.get("steps") or []:
        say = step.get("say")
        if isinstance(say, str) and say.strip():
            commands.append(
                {"action": "narrate", "text": say.strip(), "at": round(now, 3)}
            )
        for op in step.get("ops") or []:
            op = dict(op)
            action = op.get("action")
            if action == "narrate" and "at" not in op:
                op["at"] = round(now, 3)
            commands.append(op)
            if action == "play":
                now += beat_duration(op.get("animation") or {})
            elif action == "camera":
                duration = op.get("duration")
                if isinstance(duration, (int, float)) and duration > 0:
                    now += float(duration)
            elif action == "highlight":
                duration = op.get("duration", 0.4)
                hold = op.get("hold", 0.5)
                if isinstance(duration, (int, float)) and duration > 0:
                    now += float(duration)
                if isinstance(hold, (int, float)) and hold > 0:
                    now += float(hold)
        for beat in step.get("beats") or []:
            commands.append({"action": "play", "animation": beat})
            now += beat_duration(beat)
    return commands, now


def _figure_piece(label, points, color, fill_opacity, stroke_opacity):
    """One outlined, translucent polygon that starts invisible.

    `params.color` paints the fill *and* the stroke, so the two alphas are set
    explicitly; the zero opacity is what gives `fade_in` something to raise
    (a freshly created node is already opaque, so fading it in does nothing).
    """
    return [
        {
            "action": "add",
            "type": "polygon",
            "params": {"label": label, "color": color, "points": points},
        },
        {
            "action": "set",
            "target": label,
            "property": {"name": "fill_opacity", "value": fill_opacity},
        },
        {
            "action": "set",
            "target": label,
            "property": {"name": "stroke_opacity", "value": stroke_opacity},
        },
        {"action": "set", "target": label, "property": {"name": "opacity", "value": 0}},
    ]


def _figure_text(label, position):
    """A text label centred in its square, invisible until the group fades in."""
    return [
        {
            "action": "add",
            "type": "text",
            "params": {
                "label": label,
                "text": label,
                "size": 0.5,
                "position": position,
                "color": [255, 255, 255],
            },
        },
        {"action": "set", "target": label, "property": {"name": "opacity", "value": 0}},
    ]


# The classic picture: a right triangle with a square on each side.  The legs
# are 1.4 and 1.05 and the hypotenuse is exactly 1.75 (3-4-5 scaled by 0.35),
# so `a² + b² = c²` is true of the drawn figure and not only of the equation.
# The hypotenuse square is the hypotenuse vector turned a quarter turn outward.
_FIGURE_OPS = (
    _figure_piece(
        "a方",
        [[-0.7, -1.55], [0.7, -1.55], [0.7, -2.95], [-0.7, -2.95]],
        [100, 181, 246],
        0.35,
        0.9,
    )
    + _figure_piece(
        "b方",
        [[-0.7, -1.55], [-0.7, -0.5], [-1.75, -0.5], [-1.75, -1.55]],
        [255, 213, 79],
        0.35,
        0.9,
    )
    + _figure_piece(
        "c方",
        [[0.7, -1.55], [-0.7, -0.5], [0.35, 0.9], [1.75, -0.15]],
        [239, 83, 80],
        0.35,
        0.9,
    )
    + _figure_piece(
        "三角形",
        [[-0.7, -1.55], [0.7, -1.55], [-0.7, -0.5]],
        [255, 255, 255],
        0.15,
        0.9,
    )
    + _figure_piece(
        "直角标记",
        [[-0.7, -1.55], [-0.55, -1.55], [-0.55, -1.4], [-0.7, -1.4]],
        [255, 255, 255],
        0.9,
        0.0,
    )
    + _figure_text("a²", [0, -2.25])
    + _figure_text("b²", [-1.225, -1.025])
    + _figure_text("c²", [0.525, -0.325])
    + [
        {
            "action": "add",
            "type": "group",
            "params": {"label": "图形"},
            "children": ["a方", "b方", "c方", "三角形", "直角标记", "a²", "b²", "c²"],
        }
    ]
)

_FIGURE_BEATS = [
    {
        "type": "group",
        "lag_ratio": 0.08,
        "animations": [
            {"type": "fade_in", "target": label, "duration": 0.9}
            for label in ["a方", "b方", "c方", "三角形", "直角标记", "a²", "b²", "c²"]
        ],
    }
]

OFFLINE_PLAN = {
    "label": "勾股定理（离线示例）",
    "steps": [
        {
            "say": "先写下勾股定理。",
            "ops": [
                {
                    "action": "explain.equation",
                    "formula": "a^2 + b^2 = c^2",
                    "caption": "勾股定理",
                    "caption_size": 0.5,
                    "position": [0, 3.4],
                    "label": "方程",
                },
                {
                    "action": "set",
                    "target": "方程",
                    "property": {"name": "scale", "value": 0.6},
                },
            ],
            "beats": [
                {
                    "type": "scale",
                    "target": "方程",
                    "factor": 1.6666666666666667,
                    "duration": 1.2,
                }
            ],
        },
        {
            "say": "三条边各画一个正方形。",
            "ops": _FIGURE_OPS,
            "beats": _FIGURE_BEATS,
        },
        {
            "say": "看斜边上的红色正方形。",
            "beats": [
                {
                    "type": "animate",
                    "target": "c方",
                    "property": {"name": "fill_opacity", "value": 0.75},
                    "duration": 1.4,
                }
            ],
        },
        {
            "say": "直角边的平方和等于它。",
            "beats": [
                {
                    "type": "animate",
                    "target": "c方",
                    "property": {"name": "fill_opacity", "value": 0.35},
                    "duration": 1.6,
                }
            ],
        },
        {"say": "这就是勾股定理。"},
    ],
}


def accepted_plan(args):
    if args.offline:
        return OFFLINE_PLAN, "offline", ""
    if args.plan:
        with open(args.plan, "r", encoding="utf-8") as handle:
            return normalize_plan(json.load(handle)), "file", ""
    request = " ".join(args.request).strip()
    if not request:
        raise JarvisError("给一句讲解要求，例如：\"用三步动画解释勾股定理\"")
    messages = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": request},
    ]
    return messages, "model", request


def inspect_plan(probe, commands):
    """Pre-flight one candidate plan on a throwaway session.

    Returns `None` when the plan is accepted, or a sentence the model can act
    on.  Two gates, in the order a real run meets them:

    1. `validate` — every command through the same `handle` a live command
       reaches, on scratch state, so a stale label or a bad field is caught
       before anything is built;
    2. `batch` + `check` — build it for real on the throwaway session and run
       the engine's own linter, so an overlapping caption or an object off the
       frame is reported as the measurement it is rather than as a picture the
       user has to notice.

    Only `error` and `warning` findings are sent back; `info` findings are
    pacing observations, and a sub-second beat is a legitimate flourish.
    """
    reply = probe.send({"action": "validate", "commands": commands})
    if "error" in reply:
        return json.dumps(reply["error"], ensure_ascii=False)
    reply = probe.send({"action": "batch", "commands": commands})
    if "error" in reply:
        return json.dumps(reply["error"], ensure_ascii=False)
    report = probe.send({"action": "check"}).get("report") or {}
    findings = [
        finding
        for finding in report.get("findings") or []
        if finding.get("severity") in ("error", "warning")
    ]
    if not findings:
        return None
    lines = []
    for finding in findings[:6]:
        where = finding.get("label") or finding.get("target")
        lines.append(
            "%s%s：%s"
            % (finding.get("code"), "（%s）" % where if where is not None else "",
               finding.get("detail"))
        )
    return "\n".join(lines)


def plan_via_engine(spawn_probe, messages_or_plan, origin, request_text=""):
    """Ask (or read) a plan and pre-flight it through the real engine.

    `spawn_probe` returns a fresh `Client` on a throwaway session; a repair
    attempt gets a clean tree rather than the previous attempt's leftovers.
    Returns `(plan, commands, seconds)`.
    """
    if origin == "model":
        messages = messages_or_plan
        text = ""
        for attempt in range(MAX_REPAIRS + 1):
            try:
                text = ask_model(messages)
                plan = normalize_plan(extract_json(text))
            except (JarvisError, ValueError, json.JSONDecodeError) as error:
                if attempt == MAX_REPAIRS:
                    raise JarvisError("模型没有给出可解析的 JSON：%s" % error)
                print(
                    "jarvis: 模型回复不是 JSON，正在重问（%d/%d）：%s"
                    % (attempt + 1, MAX_REPAIRS, error),
                    file=sys.stderr,
                )
                messages.append({"role": "assistant", "content": text})
                messages.append(
                    {
                        "role": "user",
                        "content": "上一条不是合法 JSON（%s）。请只输出修正后的 JSON 对象，"
                        "不要 Markdown，不要解释。" % error,
                    }
                )
                text = ""
                continue
            problem = local_problem(plan)
            commands, seconds = None, 0.0
            if problem is None:
                problem = content_problem(plan, request_text)
            if problem is None:
                commands, seconds = flatten(plan)
                with spawn_probe() as probe:
                    problem = inspect_plan(probe, commands)
            if problem is None:
                return plan, commands, seconds
            if attempt == MAX_REPAIRS:
                raise JarvisError("计划被引擎拒绝（已修正 %d 次）：%s" % (MAX_REPAIRS, problem))
            print(
                "jarvis: 计划被引擎拒绝，正在修正（%d/%d）：%s"
                % (attempt + 1, MAX_REPAIRS, problem.splitlines()[0]),
                file=sys.stderr,
            )
            messages.append({"role": "assistant", "content": text})
            messages.append(
                {
                    "role": "user",
                    "content": "引擎检查上一条计划时报告了这些问题：\n%s\n"
                    "请返回修正后的完整 JSON 计划：把重叠的图形挪开或缩小、让图形留在画面内、"
                    "每句旁白配一段动画。只输出 JSON，不要解释。" % problem,
                }
            )
        raise JarvisError("计划生成失败")
    # --offline / --plan
    plan = messages_or_plan
    problem = local_problem(plan)
    if problem:
        raise JarvisError("计划结构不对：%s" % problem)
    commands, seconds = flatten(plan)
    with spawn_probe() as probe:
        problem = inspect_plan(probe, commands)
    if problem is not None:
        raise JarvisError("计划被引擎拒绝：%s" % problem)
    return plan, commands, seconds


def narration_cues(commands):
    """Every spoken line in the flattened log, in time order."""
    cues = []
    for command in commands:
        if command.get("action") != "narrate":
            continue
        text = command.get("text")
        at = command.get("at", 0.0)
        if isinstance(text, str) and text.strip():
            try:
                at = float(at)
            except (TypeError, ValueError):
                at = 0.0
            cues.append((at, text.strip()))
    cues.sort(key=lambda cue: cue[0])
    return cues


def _speak_clip(espeak, voice, text, path):
    """One line to one WAV file, the sentence on stdin.

    Args would need per-platform quoting and would put the prose in every
    process listing; `--stdin` is what `rustim-app --speak` uses for the same
    reason.
    """
    result = subprocess.run(
        [espeak, "-v", voice, "-w", path, "--stdin"],
        input=text.encode("utf-8"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0 or not os.path.exists(path) or os.path.getsize(path) == 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        raise JarvisError(
            "%s 朗读失败：%s" % (os.path.basename(espeak), detail or "没有输出")
        )


def build_narration_wav(cues, seconds, voice):
    """Place every line on a silent bed as long as the video.

    Returns the WAV path, or None when nothing could be spoken.  Clips are laid
    end to end rather than mixed: two sentences due at the same instant are a
    plan talking over itself, and a mix makes both unintelligible.  A line is
    therefore never cut off; it may start later than its cue, which the
    captions and the .srt (both timed by the engine) will not do.
    """
    espeak = shutil.which("espeak-ng") or shutil.which("espeak")
    if espeak is None:
        print(
            "jarvis: 没找到 espeak-ng/espeak，跳过配音（画面和字幕仍然完整）。",
            file=sys.stderr,
        )
        return None
    handle, wav_path = tempfile.mkstemp(prefix="rustim-voice-", suffix=".wav")
    os.close(handle)
    clips = []
    try:
        for index, (at, text) in enumerate(cues):
            clip = "%s.%d.wav" % (wav_path, index)
            try:
                _speak_clip(espeak, voice, text, clip)
                with wave.open(clip, "rb") as wav:
                    channels = wav.getnchannels()
                    width = wav.getsampwidth()
                    rate = wav.getframerate()
                    data = wav.readframes(wav.getnframes())
            except JarvisError as error:
                print("jarvis: %s（这一句跳过）" % error, file=sys.stderr)
                continue
            finally:
                if os.path.exists(clip):
                    os.remove(clip)
            if channels != 1 or width != 2:
                print(
                    "jarvis: espeak 输出不是 16 位单声道，跳过这一句。", file=sys.stderr
                )
                continue
            samples = array.array("h")
            samples.frombytes(data)
            if sys.byteorder != "little":
                samples.byteswap()
            clips.append((at, rate, samples))
        if not clips:
            os.remove(wav_path)
            return None

        rate = clips[0][1]
        usable = [clip for clip in clips if clip[1] == rate]
        if len(usable) != len(clips):
            print("jarvis: 各句采样率不一致，只混入多数的那一档。", file=sys.stderr)
        total = max(int(round(seconds * rate)), 1)
        bed = array.array("h", bytes(2 * total))
        cursor = 0
        for at, _, samples in usable:
            start = max(int(round(at * rate)), cursor)
            if start >= total:
                break
            end = min(start + len(samples), total)
            for offset in range(start, end):
                value = bed[offset] + samples[offset - start]
                bed[offset] = 32767 if value > 32767 else (-32768 if value < -32768 else value)
            cursor = end
        if sys.byteorder != "little":
            bed.byteswap()
        with wave.open(wav_path, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(rate)
            wav.writeframes(bed.tobytes())
        return wav_path
    except Exception:
        if os.path.exists(wav_path):
            os.remove(wav_path)
        raise


def mux_audio(video_path, wav_path):
    """Replace the silent export with one that carries the narration.

    Returns None on success, or a sentence describing what went wrong (the WAV
    is then left where it is rather than deleted — the voice track is the part
    that took the work).
    """
    ffmpeg = shutil.which("ffmpeg")
    if ffmpeg is None:
        return "没找到 ffmpeg，配音留在 %s" % wav_path
    directory = os.path.dirname(os.path.abspath(video_path)) or "."
    handle, temp = tempfile.mkstemp(prefix=".rustim-mux-", suffix=".mp4", dir=directory)
    os.close(handle)
    result = subprocess.run(
        [
            ffmpeg,
            "-y",
            "-loglevel",
            "error",
            "-i",
            video_path,
            "-i",
            wav_path,
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-shortest",
            temp,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        os.remove(temp)
        detail = result.stderr.decode("utf-8", "replace").strip().splitlines()
        return "ffmpeg 混音失败：%s（配音留在 %s）" % (
            detail[-1] if detail else "退出码 %d" % result.returncode,
            wav_path,
        )
    os.replace(temp, video_path)
    os.remove(wav_path)
    return None


def main(argv):
    parser = argparse.ArgumentParser(
        description="Ask a model for a rustim lecture plan and render it."
    )
    parser.add_argument("request", nargs="*", help="用一句中文描述要讲什么")
    parser.add_argument("--out", default="/tmp/jarvis.mp4", help="输出视频（或 --png 的静帧）")
    parser.add_argument("--png", action="store_true", help="写静帧而不是视频")
    parser.add_argument("--frames", type=int, default=0, help="覆盖导出帧数（默认按计划时长算）")
    parser.add_argument("--srt", default="", help="字幕路径；默认与视频同名 .srt")
    parser.add_argument("--no-srt", action="store_true", help="不写字幕")
    parser.add_argument(
        "--captions",
        dest="captions",
        action="store_true",
        default=True,
        help="把旁白画进画面（默认开启）",
    )
    parser.add_argument(
        "--no-captions", dest="captions", action="store_false", help="不把旁白画进画面"
    )
    parser.add_argument(
        "--voice",
        default=DEFAULT_VOICE,
        help="espeak-ng 的语音名（默认 cmn；macOS 的 say 用 Tingting）",
    )
    parser.add_argument("--no-speak", action="store_true", help="不给视频配音")
    parser.add_argument("--offline", action="store_true", help="不调用模型，用内置计划")
    parser.add_argument("--plan", default="", help="从文件读计划，不调用模型")
    parser.add_argument("--binary", default="", help="rustim-app 的路径")
    parser.add_argument("--print-plan", action="store_true", help="打印最终计划")
    args = parser.parse_args(argv)

    binary = find_binary(args.binary)
    if not os.path.exists(binary):
        raise JarvisError(
            "找不到二进制 %s；先运行 cargo build --release -p rustim-app，"
            "或用 --binary 指定路径。" % binary
        )

    # The request is read (and the model asked) before any process is spawned,
    # so a missing key fails in one sentence instead of after an engine start.
    request = accepted_plan(args)
    started = time.time()

    # Pre-flight on throwaway sessions: the frame count has to be on the export
    # process's command line, and it is not known until the plan has been
    # flattened.  Each probe renders one blank 1280x720 still on close.
    handle, scratch = tempfile.mkstemp(prefix="rustim-plan-", suffix=".png")
    os.close(handle)

    def spawn_probe():
        return Client.spawn(binary, ["--script", "-", "--reply", "--png", scratch])

    try:
        plan, commands, seconds = plan_via_engine(spawn_probe, *request)
    finally:
        if os.path.exists(scratch):
            os.remove(scratch)

    frames = 0
    if args.png:
        engine_args = ["--script", "-", "--reply", "--png", args.out]
        if args.captions:
            engine_args.append("--captions")
        with Client.spawn(binary, engine_args) as client:
            capabilities = client.hello()
            reply = client.send({"action": "batch", "commands": commands})
            if "error" in reply:
                raise JarvisError(
                    "执行计划时被拒绝（预检之后不应发生）：%s"
                    % json.dumps(reply["error"], ensure_ascii=False)
                )
            diff = reply.get("diff") or {}
            status = client.close()
        if seconds > 9.5:
            print(
                "警告：计划约 %.1f 秒，但 --png 固定在虚拟时间 10 秒取帧，"
                "最后的动作可能被截断；用视频模式可完整导出。" % seconds,
                file=sys.stderr,
            )
    else:
        if args.frames:
            frames = args.frames
        else:
            frames = int(math.ceil((seconds + TAIL_SECONDS) / STEP))
        frames = max(120, frames)
        if frames > MAX_FRAMES:
            print(
                "警告：%d 帧超过上限 %d，只导出前 %d 帧（约 %.0f 秒）。"
                % (frames, MAX_FRAMES, MAX_FRAMES, MAX_FRAMES * STEP),
                file=sys.stderr,
            )
            frames = MAX_FRAMES

        srt = "" if args.no_srt else (args.srt or os.path.splitext(args.out)[0] + ".srt")
        engine_args = ["--script", "-", "--reply", "--video", args.out]
        if srt:
            engine_args += ["--srt", srt]
        if args.captions:
            engine_args.append("--captions")
        engine_args += ["--frames", str(frames)]

        with Client.spawn(binary, engine_args) as client:
            capabilities = client.hello()
            reply = client.send({"action": "batch", "commands": commands})
            if "error" in reply:
                raise JarvisError(
                    "执行计划时被拒绝（预检之后不应发生）：%s"
                    % json.dumps(reply["error"], ensure_ascii=False)
                )
            diff = reply.get("diff") or {}
            status = client.close()
        if seconds + 1.0 > frames * STEP:
            print(
                "警告：计划约 %.1f 秒，导出只有 %.1f 秒，结尾被截断；"
                "用 --frames %d 重跑可完整导出。"
                % (seconds, frames * STEP, int(math.ceil((seconds + TAIL_SECONDS) / STEP))),
                file=sys.stderr,
            )

    if status != 0:
        raise JarvisError("二进制退出码 %d；输出可能不完整。" % status)

    # The engine renders frames and writes captions; it never speaks.  Sound is
    # the host's job, and this host's synthesiser is espeak-ng on the same
    # sentences the caption band drew — the cue list comes from the flattened
    # log, so the voice and the band cannot disagree about what was said.
    if not args.png and not args.no_speak:
        cues = narration_cues(commands)
        if cues:
            wav_path = build_narration_wav(cues, frames * STEP, args.voice)
            if wav_path is not None:
                problem = mux_audio(args.out, wav_path)
                if problem is None:
                    print("配音：%s 的 %d 句旁白已混入视频" % (args.voice, len(cues)))
                else:
                    print("jarvis: %s" % problem, file=sys.stderr)

    if args.print_plan:
        print(json.dumps(plan, ensure_ascii=False, indent=2))
    size = os.path.getsize(args.out) if os.path.exists(args.out) else 0
    print(
        "计划：%s（%d 步，%d 条命令，约 %.1f 秒）"
        % (plan.get("label", "(无题)"), len(plan.get("steps") or []), len(commands), seconds)
    )
    print(
        "创建 %d 个对象，修改 %d 个，删除 %d 个"
        % (
            len(diff.get("created") or []),
            len(diff.get("modified") or []),
            len(diff.get("removed") or []),
        )
    )
    print(
        "词汇：%d actions / %d shapes / %d animations"
        % (
            len(capabilities.get("actions") or []),
            len(capabilities.get("shapes") or []),
            len(capabilities.get("animations") or []),
        )
    )
    print("输出：%s（%d 字节）" % (args.out, size))
    print("用时：%.1f 秒" % (time.time() - started))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except JarvisError as error:
        print("jarvis: %s" % error, file=sys.stderr)
        sys.exit(2)
    except ProtocolError as error:
        print("jarvis: 协议错误：%s" % error, file=sys.stderr)
        sys.exit(2)
