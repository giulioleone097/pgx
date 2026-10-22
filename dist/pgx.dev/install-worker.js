// Cloudflare Worker for https://pgx.dev/install
//
// Serves the reviewed, version-controlled dist/install.sh so that
//   curl -fsSL https://pgx.dev/install | sh
// works. The script body is fetched from this repository at a PINNED ref
// (a full 40-character commit SHA) that MUST be configured
// explicitly via the INSTALL_REF var — there is deliberately no fallback to a
// mutable branch, so what runs on a user's machine is exactly what was
// reviewed. The Worker owns only the /install route.
//
// Deploy: see README.md. Set INSTALL_REF to the reviewed full commit, e.g.
//   wrangler deploy --var INSTALL_REF:<reviewed-40-character-commit-sha>
// NOT yet deployed — pending approval and Cloudflare access.
const REPO = "pgrundev/pgx";
// Only a full immutable commit ID is accepted; tags and short IDs can move or collide.
const REF_RE = /^[0-9a-f]{40}$/;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname !== "/install") {
      return new Response("not found\n", { status: 404 });
    }
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response("method not allowed\n", { status: 405 });
    }
    const ref = env && env.INSTALL_REF;
    if (!ref || !REF_RE.test(ref)) {
      // Misconfiguration must fail loudly, never silently serve a mutable ref.
      return new Response(
        "pgx installer misconfigured: INSTALL_REF must be a full 40-character commit SHA\n",
        { status: 500, headers: { "content-type": "text/plain; charset=utf-8" } }
      );
    }
    const raw = `https://raw.githubusercontent.com/${REPO}/${ref}/dist/install.sh`;
    const upstream = await fetch(raw, { cf: { cacheTtl: 300, cacheEverything: true } });
    if (!upstream.ok) {
      // Fail loudly: never serve an empty/200 body that would make `| sh` a no-op
      // (e.g. an invalid ref whose raw URL 404s).
      return new Response(`pgx installer unavailable (upstream ${upstream.status} for ref ${ref})\n`, {
        status: 502,
        headers: { "content-type": "text/plain; charset=utf-8" },
      });
    }
    const body = await upstream.text();
    return new Response(body, {
      status: 200,
      headers: {
        "content-type": "text/x-shellscript; charset=utf-8",
        "cache-control": "public, max-age=300",
        "x-pgx-install-ref": ref,
      },
    });
  },
};
