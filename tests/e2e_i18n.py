"""The agent's own text in the user's language, from the gettext catalogs
`make locale` builds (target/locale, found next to target/debug): the
system language as libc would pick it, the `language` setting overriding
it, and a change of setting translating what is already shown.

Run: e2e_i18n.py <agent binary>. Needs no display."""
import os, sys
sys.path.insert(0, os.path.dirname(__file__))
from agentclient import Agent, Steps

AGENT = sys.argv[1]
step = Steps()
NO_SESSION = {
    "en": "No graphical session was found.",
    "ja": "グラフィカルセッションが見つかりませんでした。",
    "zh": "没有找到图形会话。",
}
BAD_PORT = {
    "en": "Ports must be between 1024 and 65535 and not the discovery port 42420",
    "ja": "ポートは 1024〜65535 の範囲で、検出用のポート 42420 以外にしてください",
    "zh": "端口需在 1024–65535 之间，且不能是发现端口 42420",
}


def agent(name, **languages):
    # No desktop of any kind, and no session bus to find one through.
    env = {"WAYLAND_DISPLAY": None, "DISPLAY": None, "LANGUAGE": None, "LC_ALL": None, "LC_MESSAGES": None,
           "DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent", "RINFLOW_LOCALEDIR": None}
    env.update(languages)
    return Agent(name, f"/tmp/home-{name}", f"/tmp/run-{name}", AGENT, port=42541, env=env)


def detail(a):
    return a.state["input"]["capture"]["detail"]


def error_after_bad_port(a):
    messages = []
    a.send({"type": "update_settings", "values": {"port": 80}})
    a.pump(1.0, messages)
    return next((m["message"] for m in messages if m["type"] == "error"), None)


if __name__ == "__main__":
    a = agent("i18n-ja", LANG="ja_JP.UTF-8")
    step("LANG=ja_JP: the agent speaks Japanese", a.wait(lambda s: detail(a) == NO_SESSION["ja"], 10), detail(a))
    step("... its errors too, with the value filled in", error_after_bad_port(a) == BAD_PORT["ja"])

    a.send({"type": "update_settings", "values": {"language": "zh-CN"}})
    step("the setting wins: what is shown is translated again, to Chinese", a.wait(lambda s: detail(a) == NO_SESSION["zh"], 5), detail(a))
    step("... and new messages are in Chinese", error_after_bad_port(a) == BAD_PORT["zh"])

    a.send({"type": "update_settings", "values": {"language": "en-US"}})
    step("English", a.wait(lambda s: detail(a) == NO_SESSION["en"], 5), detail(a))

    a.send({"type": "update_settings", "values": {"language": "ja-JP"}})
    step("Japanese chosen in the setting", a.wait(lambda s: detail(a) == NO_SESSION["ja"], 5), detail(a))
    a.send({"type": "update_settings", "values": {"language": "fr-FR"}})
    a.pump(1.0)
    step("a language Rinflow doesn't have is refused", a.state["settings"]["language"] == "ja-JP")
    a.send({"type": "update_settings", "values": {"language": "system"}})
    step("back to the system's language", a.wait(lambda s: s["settings"]["language"] == "system" and detail(a) == NO_SESSION["ja"], 5))
    a.quit()
    step("agent exits cleanly", a.proc.returncode == 0)

    # The language list as libc reads it: LANGUAGE first, in order, skipping
    # languages without a catalog (Traditional Chinese gets no Simplified).
    b = agent("i18n-list", LANG="en_US.UTF-8", LANGUAGE="zh_TW:ja:en")
    step("LANGUAGE=zh_TW:ja:en: no zh_TW catalog, so Japanese", b.wait(lambda s: detail(b) == NO_SESSION["ja"], 10), detail(b))
    b.quit()
    c = agent("i18n-c", LANG="C", LANGUAGE="ja")
    step("in the C locale LANGUAGE is ignored, as libc does: English", c.wait(lambda s: detail(c) == NO_SESSION["en"], 10), detail(c))
    c.quit()
    d = agent("i18n-zh", LC_MESSAGES="zh_CN.UTF-8", LANG="en_US.UTF-8")
    step("LC_MESSAGES=zh_CN over LANG: Chinese", d.wait(lambda s: detail(d) == NO_SESSION["zh"], 10), detail(d))
    d.quit()
    sys.exit(step.finish())
