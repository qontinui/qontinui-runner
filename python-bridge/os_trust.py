"""Outbound TLS in the bridge trusts the OS certificate store.

Imported FIRST by every bridge entry point the runner spawns, for its side
effect: ``truststore.inject_into_ssl()`` makes ``ssl.SSLContext`` verify
against the operating system's trust store (SChannel on Windows, the Security
framework on macOS, the system CA store on Linux) instead of ``certifi``'s
bundled Mozilla roots. That is what lets the bridge's outbound calls
(``requests``, ``httpx``, ``aiohttp``, model downloads) succeed behind a
TLS-inspecting corporate proxy whose root IT installed into the OS store.

It must run before any library builds an SSL context — ``aiohttp`` builds its
default contexts at import time — which is why it is an import-time side
effect placed ahead of every other import, not a call inside ``main()``.

Plan ``2026-10-10-spec-front-end-phase-9-generic-boundary``, Phase 5 (C3).
The corporate root must therefore be in the OS store. The runner deliberately
does NOT export ``SSL_CERT_FILE`` from the profile's ``network.ca_bundle``: on
Linux that variable REPLACES the system trust store (OpenSSL, which
``truststore`` uses there, reads only the named file), so it would remove the
public roots rather than add the corporate one. ``network.ca_bundle`` reaches
Node only, through the additive ``NODE_EXTRA_CA_CERTS``.
"""

from __future__ import annotations

import logging

_LOG = logging.getLogger(__name__)


def inject_os_trust() -> bool:
    """Route ``ssl.SSLContext`` through the OS trust store.

    Returns ``True`` when injected. ``False`` (with a WARNING naming the
    consequence) only when ``truststore`` is missing from the environment,
    which is a broken install: it is a declared dependency.
    """
    try:
        import truststore
    except ImportError:
        _LOG.warning(
            "truststore is not installed: outbound TLS in the bridge falls back to "
            "certifi's bundled roots, so a corporate root in the OS store is NOT trusted"
        )
        return False
    truststore.inject_into_ssl()
    return True


INJECTED = inject_os_trust()
