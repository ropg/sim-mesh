"""No test reaches a host outside this machine. A test serves what it
fetches from a host of its own on loopback; one that looked up a real
host would hang on a slow link, and pass or fail with the host, so it
fails at once instead, naming the host."""

import socket

import pytest

LOCAL = ("localhost", "127.0.0.1", "::1", "0.0.0.0", "")


@pytest.fixture(autouse=True)
def no_outside_network(monkeypatch):
    real = socket.getaddrinfo

    def getaddrinfo(host, *args, **kwargs):
        name = host.decode() if isinstance(host, bytes) else (host or "")
        if name not in LOCAL and not name.startswith("127.") and not name.endswith(".localhost"):
            raise socket.gaierror(socket.EAI_NONAME, "a test may not reach %s" % name)
        return real(host, *args, **kwargs)
    monkeypatch.setattr(socket, "getaddrinfo", getaddrinfo)
