// Temporary SDK proxy: proxies SDK control requests to the runner's IPC control endpoints
// This allows the SDK connection to use the runner's own UI elements
import http from "http";

const RUNNER_HOST = "127.0.0.1";
const RUNNER_PORT = 9876;
const PROXY_PORT = 19876;
const MAX_BODY_SIZE = 10 * 1024 * 1024; // 10 MB

const healthResponse = JSON.stringify({
  success: true,
  data: { responsive: true },
  uiBridge: {
    appId: "qontinui-runner-proxy",
    appName: "Qontinui Runner (IPC Proxy)",
    appType: "desktop",
    framework: "tauri",
    capabilities: ["control", "renderLog", "debug"],
  },
});

const server = http.createServer((req, res) => {
  // Health endpoint
  if (req.url === "/health" || req.url === "/control/health") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(healthResponse);
    return;
  }

  // Fire-and-forget operations: return success immediately to avoid
  // disrupting the runner's IPC during page navigation/refresh
  if (req.url === "/control/page/navigate" || req.url === "/control/page/refresh") {
    let body = "";
    let bodySize = 0;
    req.on("data", (chunk) => {
      bodySize += chunk.length;
      if (bodySize > MAX_BODY_SIZE) {
        req.destroy();
        res.writeHead(413, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ success: false, error: "Request body too large" }));
        return;
      }
      body += chunk;
    });
    req.on("end", () => {
      const action = req.url.includes("navigate") ? "pageNavigate" : "pageRefresh";
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          success: true,
          data: { success: true, fireAndForget: true, action, timestamp: Date.now() },
          timestamp: Date.now(),
        }),
      );

      // Also trigger internal navigation for the runner's own SPA routing
      if (action === "pageNavigate" && body) {
        try {
          const parsed = JSON.parse(body);
          if (parsed.url) {
            const urlPath = new URL(parsed.url).pathname.replace(/^\//, "");
            if (urlPath) {
              const navReq = http.request(
                {
                  hostname: RUNNER_HOST,
                  port: RUNNER_PORT,
                  path: "/navigate",
                  method: "POST",
                  headers: { "Content-Type": "application/json" },
                  timeout: 5000,
                },
                () => {},
              );
              navReq.on("error", () => {}); // ignore errors
              navReq.end(JSON.stringify({ page: urlPath }));
            }
          }
        } catch {
          /* ignore parse errors */
        }
      }
    });
    return;
  }

  // Proxy requests to runner's /ui-bridge/control/*
  // SDK handler sends paths like /ai/search, /elements, /snapshot etc.
  // These need to be mapped to /ui-bridge/control/ai/search, /ui-bridge/control/elements, etc.
  const urlPath = req.url.split("?")[0];
  const query = req.url.includes("?") ? req.url.substring(req.url.indexOf("?")) : "";
  const controlPath = urlPath.startsWith("/control/") ? urlPath : "/control" + urlPath;
  const targetPath = "/ui-bridge" + controlPath + query;

  let body = "";
  let bodySize = 0;
  req.on("data", (chunk) => {
    bodySize += chunk.length;
    if (bodySize > MAX_BODY_SIZE) {
      req.destroy();
      res.writeHead(413, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ success: false, error: "Request body too large" }));
      return;
    }
    body += chunk;
  });
  req.on("end", () => {
    const options = {
      hostname: RUNNER_HOST,
      port: RUNNER_PORT,
      path: targetPath,
      method: req.method,
      headers: { ...req.headers, host: `${RUNNER_HOST}:${RUNNER_PORT}` },
      timeout: 15000,
    };

    const isAiEndpoint = urlPath.includes("/ai/");
    const proxyReq = http.request(options, (proxyRes) => {
      if (isAiEndpoint) {
        // For AI endpoints, unwrap the ApiResponse {success, data} wrapper
        // so callers get {results, bestMatch, ...} directly
        let responseBody = "";
        proxyRes.on("data", (chunk) => {
          responseBody += chunk;
        });
        proxyRes.on("end", () => {
          try {
            const json = JSON.parse(responseBody);
            if (
              json.success &&
              json.data &&
              typeof json.data === "object" &&
              !Array.isArray(json.data)
            ) {
              const unwrapped = JSON.stringify(json.data);
              const headers = { ...proxyRes.headers };
              headers["content-length"] = Buffer.byteLength(unwrapped);
              delete headers["transfer-encoding"];
              res.writeHead(proxyRes.statusCode, headers);
              res.end(unwrapped);
            } else {
              res.writeHead(proxyRes.statusCode, proxyRes.headers);
              res.end(responseBody);
            }
          } catch {
            res.writeHead(proxyRes.statusCode, proxyRes.headers);
            res.end(responseBody);
          }
        });
      } else {
        res.writeHead(proxyRes.statusCode, proxyRes.headers);
        proxyRes.pipe(res);
      }
    });

    proxyReq.on("error", (err) => {
      res.writeHead(502, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ success: false, error: err.message }));
    });

    proxyReq.on("timeout", () => {
      proxyReq.destroy();
      res.writeHead(504, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ success: false, error: "Proxy timeout" }));
    });

    if (body) proxyReq.write(body);
    proxyReq.end();
  });
});

server.listen(PROXY_PORT, "127.0.0.1", () => {
  console.log(`SDK proxy listening on http://127.0.0.1:${PROXY_PORT}`);
});
