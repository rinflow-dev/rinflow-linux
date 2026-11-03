"""A fakedesk process (tests/fakedesk) and a client of its control socket,
for the end-to-end tests."""
import atexit, json, os, signal, socket, subprocess, time


def session_bus():
    """A private session bus; the fake desktop owns the portal name on it.
    The bus forks away from the test, so it is stopped when the test exits."""
    out = subprocess.check_output(["dbus-daemon", "--session", "--print-address", "--print-pid", "--fork"]).decode().split()
    address, pid = out[0], int(out[1])
    atexit.register(lambda: _stop(pid))
    return address


def _stop(pid):
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass


class Fake:
    def __init__(self, binary, name, bus, control, zones, extra=()):
        self.log = open(f"/tmp/{name}.log", "w")
        env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=bus, RUST_LOG="info")
        self.proc = subprocess.Popen([binary, "--control", control, "--zones", zones, *extra], env=env,
                                     stdout=self.log, stderr=subprocess.STDOUT)
        for _ in range(50):
            try:
                self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.s.connect(control)
                break
            except OSError:
                time.sleep(0.1)
        else:
            raise RuntimeError(f"{name}: control socket never appeared")
        self.f = self.s.makefile("rwb", buffering=0)

    def cmd(self, **kw):
        self.s.sendall(json.dumps(kw).encode() + b"\n")
        return json.loads(self.f.readline())

    def state(self):
        return self.cmd(cmd="state")["state"]

    def wait(self, predicate, seconds=20):
        end = time.time() + seconds
        while time.time() < end:
            if predicate(self.state()):
                return True
            time.sleep(0.2)
        return False

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.log.close()
