#!/usr/bin/env python3
"""Probe both loopback listeners in a process that never forks.

The parent harness must not hold these sockets: a concurrent fork in another
test thread can inherit them and keep the port busy after the parent closes it.
Exit 0 means available, 1 means busy/unavailable, and 2 means a probe error.
"""
import errno
import socket
import sys


def probe(port):
    try:
        with socket.socket(socket.AF_INET) as ipv4:
            ipv4.bind(("127.0.0.1", port))
            ipv4.listen(1)
            try:
                with socket.socket(socket.AF_INET6) as ipv6:
                    ipv6.bind(("::1", port))
                    ipv6.listen(1)
            except OSError as error:
                if error.errno not in (errno.EAFNOSUPPORT, errno.EPROTONOSUPPORT, errno.EADDRNOTAVAIL):
                    raise
        return 0
    except OSError as error:
        if error.errno == errno.EADDRINUSE:
            return 1
        print("loopback port probe failed: %s" % error, file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(probe(int(sys.argv[1])))
