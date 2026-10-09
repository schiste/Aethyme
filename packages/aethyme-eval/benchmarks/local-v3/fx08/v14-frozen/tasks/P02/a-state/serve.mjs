import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, resolve, sep } from "node:path";

const root = resolve(process.cwd());
const mime = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".json", "application/json; charset=utf-8"]
]);
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url ?? "/", "http://127.0.0.1");
    const relative = decodeURIComponent(url.pathname === "/" ? "/index.html" : url.pathname);
    const file = resolve(root, `.${relative}`);
    if (file !== root && !file.startsWith(root + sep)) {
      response.writeHead(403).end("Forbidden");
      return;
    }
    const content = await readFile(file);
    response.writeHead(200, {
      "content-type": mime.get(extname(file)) ?? "application/octet-stream",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff"
    });
    response.end(content);
  } catch {
    response.writeHead(404).end("Not found");
  }
});
const port = Number(process.env.PORT ?? 4173);
server.listen(port, "127.0.0.1", () => {
  console.log(`Playground listening on http://127.0.0.1:${port}`);
});
