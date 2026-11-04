"""The agent's text goes to the window untranslated (protocol 2): a message
id with its values, which the window translates where it shows it. So what
the agent sends doesn't depend on its environment's language nor on the
`language` setting, which the agent only keeps (and checks) for the window.
Which catalog the window picks, and the translation itself, are unit-tested
in shell/src/i18n.rs.

Run: e2e_i18n.py <agent binary>. Needs no display."""
import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps

AGENT = sys.argv[1]
step = Steps()
NO_SESSION = {"id": "No graphical session was found."}
BAD_PORT = {"id": "Ports must be between 1024 and 65535 and not the discovery port {port}", "args": {"port": "42420"}}
BAD_LANGUAGE = {"id": "Language must be system, Simplified Chinese, Japanese, or English"}


def agent(name, **languages):
    # No desktop of any kind, and no session bus to find one through.
    env = {"WAYLAND_DISPLAY": None, "DISPLAY": None, "LANGUAGE": None, "LC_ALL": None, "LC_MESSAGES": None,
           "DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent", "RINFLOW_LOCALEDIR": None}
    env.update(languages)
    return Agent(name, f"/tmp/home-{name}", f"/tmp/run-{name}", AGENT, port=42541, env=env)


def detail(a):
    return a.state["input"]["capture"]["detail"]


def error_after(a, values):
    messages = []
    a.send({"type": "update_settings", "values": values})
    a.pump(1.0, messages)
    return next((m["message"] for m in messages if m["type"] == "error"), None)


if __name__ == "__main__":
    for name, languages in [
        ("i18n-ja", {"LANG": "ja_JP.UTF-8"}),
        ("i18n-list", {"LANG": "en_US.UTF-8", "LANGUAGE": "zh_TW:ja:en"}),
        ("i18n-zh", {"LC_MESSAGES": "zh_CN.UTF-8", "LANG": "en_US.UTF-8"}),
    ]:
        a = agent(name, **languages)
        step(f"{languages}: the message goes out as its id", a.wait(lambda s: detail(a) == NO_SESSION, 10), detail(a))
        a.quit()

    a = agent("i18n-setting", LANG="ja_JP.UTF-8")
    a.wait(lambda s: detail(a) == NO_SESSION, 10)
    step("an error carries its id and values, untranslated", error_after(a, {"port": 80}) == BAD_PORT)
    a.send({"type": "update_settings", "values": {"language": "zh-CN"}})
    step("the setting is kept for the window", a.wait(lambda s: s["settings"]["language"] == "zh-CN", 5))
    a.pump(0.5)
    step("... and changes nothing the agent sends", detail(a) == NO_SESSION, detail(a))
    step("a language Rinflow doesn't have is refused", error_after(a, {"language": "fr-FR"}) == BAD_LANGUAGE)
    step("... and the setting stays", a.state["settings"]["language"] == "zh-CN")
    a.send({"type": "update_settings", "values": {"language": "system"}})
    step("back to the system's language", a.wait(lambda s: s["settings"]["language"] == "system", 5))
    a.quit()
    step("agent exits cleanly", a.proc.returncode == 0)
    sys.exit(step.finish())
