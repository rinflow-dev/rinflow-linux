"""An agent process plus a client of its socket, for the end-to-end tests."""
import json, os, shutil, socket, subprocess, time


class Agent:
    def __init__(self, name, home, runtime, agent_binary, port=None, env=None, fresh=True):
        self.name = name
        # A fresh device every run: identity, pairings and layout are kept
        # across restarts by design, which a repeated test must not inherit.
        # (`fresh=False` when the caller made the directories itself.)
        if fresh:
            shutil.rmtree(home, ignore_errors=True)
            shutil.rmtree(runtime, ignore_errors=True)
        self.env = dict(os.environ, HOME=home, XDG_RUNTIME_DIR=runtime,
                        XDG_DATA_HOME=f"{home}/.local/share", XDG_STATE_HOME=f"{home}/.local/state",
                        RUST_LOG="info", LANG="en_US.UTF-8")
        if env:
            self.env.update(env)
        os.makedirs(runtime, exist_ok=True)
        data = f"{home}/.local/share/rinflow"
        os.makedirs(data, exist_ok=True)
        if port:
            with open(f"{data}/settings.json", "w") as f:
                json.dump({"port": port}, f)
        self.log = open(f"/tmp/{name}.log", "w")
        self.proc = subprocess.Popen([agent_binary], env=self.env, stdout=self.log, stderr=subprocess.STDOUT)
        self.sock_path = f"{runtime}/rinflow/agent.sock"
        self.state = None
        self.buf = b""
        for _ in range(50):
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(self.sock_path)
                break
            except OSError:
                time.sleep(0.2)
        else:
            raise RuntimeError(f"{name}: agent socket never appeared")
        self.s.settimeout(0.5)
        hello = self.read()
        assert hello["type"] == "hello", hello
        self.send({"type": "hello", "protocol": 1})
        self.pump(1.0)

    def read(self):
        while b"\n" not in self.buf:
            chunk = self.s.recv(65536)
            if not chunk:
                return None
            self.buf += chunk
        line, self.buf = self.buf.split(b"\n", 1)
        return json.loads(line)

    def send(self, obj):
        self.s.sendall(json.dumps(obj).encode() + b"\n")

    def pump(self, seconds, collect=None):
        end = time.time() + seconds
        while time.time() < end:
            try:
                msg = self.read()
            except socket.timeout:
                continue
            if msg is None:
                return
            if msg["type"] == "state":
                self.state = msg
            elif msg["type"] == "error":
                print(f"  [{self.name}] error: {msg['message']}", flush=True)
            if collect is not None:
                collect.append(msg)

    def wait(self, predicate, seconds=30):
        end = time.time() + seconds
        while time.time() < end:
            self.pump(0.3)
            if self.state and predicate(self.state):
                return True
        return False

    def quit(self):
        try:
            self.send({"type": "quit"})
            self.pump(1.0)
        finally:
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            self.log.close()


class Steps:
    def __init__(self):
        self.failed = []

    def __call__(self, name, ok, detail=""):
        print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail else ""), flush=True)
        if not ok:
            self.failed.append(name)

    def finish(self):
        print()
        print("RESULT:", "ALL PASSED" if not self.failed else f"{len(self.failed)} FAILED: {self.failed}")
        return 1 if self.failed else 0
