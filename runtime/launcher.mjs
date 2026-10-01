// Runs the official code-server release behind the POM's authenticated plugin proxy.
// The plugin host launches this process with private paths and reads exactly one
// JSON status line from stdout; diagnostics are written only to stderr.

import { spawn } from "node:child_process";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { existsSync, mkdirSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const env = process.env;
const codeRoot = env.POM_CODE_SERVER_ROOT || "";
const workspace = env.POM_CODE_SERVER_WORKSPACE || "";
const dataDir = env.POM_CODE_SERVER_DATA_DIR || "";
const version = env.POM_CODE_SERVER_VERSION || "unknown";
const windows = process.platform === "win32";
const nodeBin = join(codeRoot, "lib", windows ? "node.exe" : "node");
const token = randomBytes(32).toString("base64url");
const TOKEN_HEADER = "x-pom-plugin-token";
const STATUS_PATH = "/_pom/status";
const RESTART_PATH = "/_pom/restart";

let proxyServer;
let codeServer;
let codeServerPort;
let restartInProgress = false;
let shuttingDown = false;
let runtimeStatus = { status: "starting" };
let reported = false;

function log(message) {
  process.stderr.write(`code-server: ${message}\n`);
}

function report(value) {
  if (reported) return;
  reported = true;
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

export function tokenMatches(candidate, expected) {
  if (typeof candidate !== "string") return false;
  const left = Buffer.from(candidate);
  const right = Buffer.from(expected);
  return left.length === right.length && timingSafeEqual(left, right);
}

export function prefixedLocation(location, prefix, forwardedHost) {
  if (typeof location !== "string" || !prefix) return location;
  if (/^https?:\/\//i.test(location)) {
    try {
      const url = new URL(location);
      if (forwardedHost && url.host.toLowerCase() !== forwardedHost.toLowerCase()) return location;
      if (!url.pathname.startsWith(`${prefix}/`) && url.pathname !== prefix) url.pathname = `${prefix}${url.pathname}`;
      return url.toString();
    } catch {
      return location;
    }
  }
  if (!location.startsWith("/") || location.startsWith(`${prefix}/`) || location === prefix) return location;
  return `${prefix}${location}`;
}

export function proxyResponseHeaders(headers, prefix, forwardedHost) {
  const result = { ...headers };
  for (const key of ["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade", "set-cookie"]) {
    delete result[key];
  }
  if (result.location) result.location = prefixedLocation(result.location, prefix, forwardedHost);
  if (result["service-worker-allowed"] && prefix) result["service-worker-allowed"] = `${prefix}/`;
  return result;
}

async function freePort() {
  const listener = net.createServer();
  await new Promise((resolve, reject) => {
    listener.once("error", reject);
    listener.listen(0, "127.0.0.1", resolve);
  });
  const port = listener.address().port;
  await new Promise((resolve, reject) => listener.close((error) => error ? reject(error) : resolve()));
  return port;
}

function serverEnvironment() {
  const output = {};
  const safe = ["LANG", "LC_ALL", "LC_CTYPE", "TZ", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP"];
  for (const name of safe) if (env[name] !== undefined) output[name] = env[name];
  const delimiter = windows ? ";" : ":";
  const systemPath = env.PATH || (windows ? "C:\\Windows\\System32" : "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
  output.PATH = [join(codeRoot, "bin"), join(codeRoot, "lib"), systemPath].join(delimiter);
  output.HOME = join(dataDir, "home");
  output.USERPROFILE = output.HOME;
  output.XDG_CONFIG_HOME = join(output.HOME, ".config");
  output.XDG_DATA_HOME = join(output.HOME, ".local", "share");
  output.TERM = "xterm-256color";
  output.COLORTERM = "truecolor";
  output.CODE_SERVER_DISABLE_TELEMETRY = "1";
  if (!windows) output.SHELL = env.POM_CODE_SERVER_SHELL || "/bin/bash";
  return output;
}

function waitForExit(child) {
  if (child.exitCode !== null) return Promise.resolve();
  return new Promise((resolve) => child.once("exit", resolve));
}

async function stopCodeServer() {
  const child = codeServer;
  codeServer = undefined;
  codeServerPort = undefined;
  if (!child || child.exitCode !== null) return;
  log("stopping current code-server process");
  child.kill("SIGTERM");
  const timedOut = await Promise.race([
    waitForExit(child).then(() => false),
    new Promise((resolve) => setTimeout(() => resolve(true), 4000)),
  ]);
  if (timedOut && child.exitCode === null) {
    child.kill("SIGKILL");
    await Promise.race([waitForExit(child), new Promise((resolve) => setTimeout(resolve, 2000))]);
  }
  log("code-server process stopped");
}

async function startCodeServer() {
  log("starting bundled code-server");
  if (!codeRoot || !workspace || !dataDir || !existsSync(nodeBin)) {
    throw new Error("the bundled code-server runtime is incomplete");
  }
  for (const directory of [workspace, dataDir, join(dataDir, "home"), join(dataDir, "user-data"), join(dataDir, "extensions")]) {
    mkdirSync(directory, { recursive: true });
  }

  const port = await freePort();
  const userData = join(dataDir, "user-data");
  const extensions = join(dataDir, "extensions");
  const args = [
    codeRoot,
    `--bind-addr=127.0.0.1:${port}`,
    "--auth=none",
    "--disable-telemetry",
    "--disable-update-check",
    "--disable-workspace-trust",
    "--user-data-dir",
    userData,
    "--extensions-dir",
    extensions,
    "--ignore-last-opened",
    workspace,
  ];
  const child = spawn(nodeBin, args, {
    cwd: workspace,
    env: serverEnvironment(),
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  codeServer = child;
  codeServerPort = port;
  child.stdout.on("data", (chunk) => process.stderr.write(`code-server: ${chunk}`));
  child.stderr.on("data", (chunk) => process.stderr.write(`code-server: ${chunk}`));
  child.on("error", (error) => {
    if (codeServer === child) runtimeStatus = { status: "error", error: error.message };
  });
  child.on("exit", (code, signal) => {
    if (codeServer === child && !shuttingDown) {
      codeServer = undefined;
      codeServerPort = undefined;
      runtimeStatus = { status: "error", error: `code-server exited (${code ?? signal ?? "unknown"})` };
      log(runtimeStatus.error);
    }
  });

  const deadline = Date.now() + 120_000;
  let lastError = "not ready";
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`code-server exited before becoming ready (${child.exitCode})`);
    try {
      const response = await fetch(`http://127.0.0.1:${port}/healthz`, { signal: AbortSignal.timeout(1500) });
      if (response.ok) {
        runtimeStatus = { status: "ready", detail: { version } };
        log(`v${version} ready on 127.0.0.1:${port}`);
        return;
      }
      lastError = `health check returned HTTP ${response.status}`;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error(`code-server did not become ready: ${lastError}`);
}

async function restartCodeServer() {
  if (restartInProgress || shuttingDown) return;
  restartInProgress = true;
  log("code-server restart requested");
  runtimeStatus = { status: "starting" };
  await stopCodeServer();
  log("previous code-server process stopped");
  try {
    await startCodeServer();
  } catch (error) {
    runtimeStatus = { status: "error", error: error instanceof Error ? error.message : String(error) };
    log(runtimeStatus.error);
    await stopCodeServer();
  } finally {
    restartInProgress = false;
  }
}

function authorized(request) {
  return tokenMatches(request.headers[TOKEN_HEADER], token);
}

function sendJson(response, status, value) {
  const body = Buffer.from(JSON.stringify(value));
  response.writeHead(status, {
    "content-type": "application/json; charset=utf-8",
    "content-length": body.length,
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  response.end(body);
}

function outgoingHeaders(incoming, proxyPort, isUpgrade = false) {
  const headers = { ...incoming };
  delete headers.host;
  delete headers[TOKEN_HEADER];
  if (!isUpgrade) {
    for (const name of ["connection", "keep-alive", "proxy-connection", "upgrade", "transfer-encoding"]) delete headers[name];
  } else {
    headers.connection = "Upgrade";
    headers.upgrade = incoming.upgrade || "websocket";
  }
  headers.host = incoming["x-forwarded-host"] || `127.0.0.1:${proxyPort}`;
  return headers;
}

function proxyHttp(request, response, proxyPort) {
  if (!codeServer || codeServer.exitCode !== null || !codeServerPort) {
    sendJson(response, 503, runtimeStatus);
    return;
  }
  const prefix = typeof request.headers["x-forwarded-prefix"] === "string" ? request.headers["x-forwarded-prefix"] : "";
  const outbound = http.request({
    hostname: "127.0.0.1",
    port: codeServerPort,
    method: request.method,
    path: request.url,
    headers: outgoingHeaders(request.headers, proxyPort),
  }, (upstream) => {
    response.writeHead(upstream.statusCode || 502, proxyResponseHeaders(upstream.headers, prefix, request.headers["x-forwarded-host"]));
    upstream.pipe(response);
  });
  outbound.on("error", (error) => {
    log(`proxy ${request.method} ${request.url}: ${error.message}`);
    if (!response.headersSent) sendJson(response, 502, { error: "code-server is unavailable" });
    else response.destroy(error);
  });
  request.on("aborted", () => outbound.destroy());
  request.pipe(outbound);
}

function websocketResponseHead(response) {
  const lines = [`HTTP/1.1 ${response.statusCode || 502} ${response.statusMessage || "Bad Gateway"}`];
  for (let index = 0; index < response.rawHeaders.length; index += 2) {
    const name = response.rawHeaders[index];
    const value = response.rawHeaders[index + 1];
    if (name.toLowerCase() !== "set-cookie") lines.push(`${name}: ${value}`);
  }
  return Buffer.from(`${lines.join("\r\n")}\r\n\r\n`);
}

function proxyWebSocket(request, clientSocket, head, proxyPort) {
  if (!authorized(request) || !codeServer || codeServer.exitCode !== null || !codeServerPort) {
    clientSocket.end("HTTP/1.1 401 Unauthorized\r\nconnection: close\r\n\r\n");
    return;
  }
  const outbound = http.request({
    hostname: "127.0.0.1",
    port: codeServerPort,
    method: request.method,
    path: request.url,
    headers: outgoingHeaders(request.headers, proxyPort, true),
  });
  outbound.on("upgrade", (upstream, upstreamSocket, upstreamHead) => {
    if (upstream.statusCode !== 101) {
      clientSocket.end(websocketResponseHead(upstream));
      upstreamSocket.destroy();
      return;
    }
    clientSocket.write(websocketResponseHead(upstream));
    if (upstreamHead.length) clientSocket.write(upstreamHead);
    if (head.length) upstreamSocket.write(head);
    clientSocket.pipe(upstreamSocket);
    upstreamSocket.pipe(clientSocket);
    clientSocket.on("error", () => upstreamSocket.destroy());
    clientSocket.on("close", () => upstreamSocket.destroy());
    upstreamSocket.on("error", () => clientSocket.destroy());
    upstreamSocket.on("close", () => clientSocket.destroy());
  });
  outbound.on("response", (upstream) => {
    clientSocket.end(websocketResponseHead(upstream));
  });
  outbound.on("error", (error) => {
    log(`websocket ${request.url}: ${error.message}`);
    clientSocket.end("HTTP/1.1 502 Bad Gateway\r\nconnection: close\r\n\r\n");
  });
  outbound.end();
}

function startProxyServer() {
  const server = http.createServer((request, response) => {
    if (!authorized(request)) {
      sendJson(response, 401, { error: "unauthorized" });
      return;
    }
    const pathname = new URL(request.url, "http://code-server.invalid").pathname;
    if (pathname === STATUS_PATH && request.method === "GET") {
      sendJson(response, 200, runtimeStatus);
      return;
    }
    if (pathname === RESTART_PATH && request.method === "POST") {
      sendJson(response, 202, { status: "starting" });
      void restartCodeServer();
      return;
    }
    proxyHttp(request, response, server.address().port);
  });
  server.on("upgrade", (request, socket, head) => proxyWebSocket(request, socket, head, server.address().port));
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.removeListener("error", reject);
      resolve(server);
    });
  });
}

async function shutdown(code = 0) {
  if (shuttingDown) return;
  shuttingDown = true;
  runtimeStatus = { status: "stopping" };
  if (proxyServer) proxyServer.close();
  await stopCodeServer();
  process.exit(code);
}

async function main() {
  proxyServer = await startProxyServer();
  report({ status: "ready", port: proxyServer.address().port, token, detail: { version } });
  void restartCodeServer();
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.stdin.on("end", () => void shutdown(0));
  process.stdin.on("error", () => void shutdown(0));
  process.stdin.resume();
  for (const signal of ["SIGTERM", "SIGINT"]) process.on(signal, () => void shutdown(0));
  main().catch((error) => {
    const message = error instanceof Error ? error.message : String(error);
    log(message);
    report({ status: "error", error: message });
    void shutdown(1);
  });
}
