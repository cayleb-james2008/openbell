// House-frontend build for a Rust app: React + Tailwind 4, the library parts live in ../rune/used,
// producing a lean initial shell at ../assets/index.html (+ .gz) and lazy-loaded routes under ../assets/chunks/
// compiled directly into the Rust binary via ../src/embedded_chunks.rs.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const root = import.meta.dirname;
const used = path.resolve(root, "../rune/used");

// "@/x" is the app's own src/x first, then the copied library part rune/used/x.
function runeUsed(): Plugin {
  const exts = ["", ".tsx", ".ts", ".css", "/index.tsx", "/index.ts"];
  return {
    name: "rune-used",
    enforce: "pre",
    resolveId(id) {
      if (!id.startsWith("@/")) return null;
      for (const base of [path.resolve(root, "src"), used]) {
        for (const e of exts) {
          const p = path.join(base, id.slice(2) + e);
          if (fs.existsSync(p) && fs.statSync(p).isFile()) return p;
        }
      }
      return null;
    },
  };
}

function contentTypeFor(file: string): string {
  if (file.endsWith(".js")) return "application/javascript; charset=utf-8";
  if (file.endsWith(".css")) return "text/css; charset=utf-8";
  if (file.endsWith(".woff2")) return "font/woff2";
  if (file.endsWith(".svg")) return "image/svg+xml";
  return "application/octet-stream";
}

// Inline only the entry script & stylesheet into ../assets/index.html, emit lazy chunks & fonts into
// ../assets/chunks/ (+ .gz), enforce zero back-imports from lazy chunks to the entry script, and generate ../src/embedded_chunks.rs.
function singleFile(): Plugin {
  return {
    name: "single-file",
    enforce: "post",
    closeBundle() {
      const dist = path.resolve(root, "dist");
      let html = fs.readFileSync(path.join(dist, "index.html"), "utf8");
      const inlinedFiles = new Set<string>();
      let entryJsBasename = "";

      let modulePreloads: string[] = [];

      html = html.replace(/<script[^>]*src="([^"]+)"[^>]*><\/script>/g, (_m, src) => {
        const rel = src.replace(/^\/+/, "").replace(/^\.\//, "");
        inlinedFiles.add(rel);
        entryJsBasename = path.basename(rel);
        let js = fs.readFileSync(path.join(dist, rel), "utf8");
        // Entry script is inlined into `/` so relative `./<chunk>.js` imports must resolve under `/chunks/`.
        js = js.replace(/\bimport\((["'`])\.\/([^"'`]+\.js)\1\)/g, 'import("/chunks/$2")');
        js = js.replace(/(\bfrom\s*)(["'`])\.\/([^"'`]+\.js)\2/g, '$1"/chunks/$3"');
        js = js.replace(/(\bimport\s*)(["'`])\.\/([^"'`]+\.js)\2/g, '$1"/chunks/$3"');
        if (/(?:\bfrom\s*|\bimport\s*\(?\s*)["'`]\.\//.test(js)) {
          throw new Error(`Entry script ${entryJsBasename} still contains relative ./ chunk import`);
        }
        const staticMatches = [...js.matchAll(/(?:\bfrom|\bimport)\s*["'`](\/chunks\/[^"'`]+\.js)["'`]/g)].map((m) => m[1]);
        modulePreloads = [...new Set(staticMatches)];
        js = js.replace(/<\/script/gi, "<\\/script");
        return `<script type="module">${js}</script>`;
      });

      if (modulePreloads.length > 0) {
        const preloadLinks = modulePreloads.map((href) => `<link rel="modulepreload" href="${href}">`).join("");
        html = html.replace(/<\/head>/i, `${preloadLinks}</head>`);
      }

      html = html.replace(/<link[^>]*rel="stylesheet"[^>]*href="([^"]+)"[^>]*>/g, (_m, href) => {
        const rel = href.replace(/^\/+/, "").replace(/^\.\//, "");
        inlinedFiles.add(rel);
        const css = fs.readFileSync(path.join(dist, rel), "utf8");
        return `<style>${css}</style>`;
      });

      // The Hallmark stamp is the first line of the page CSS (the lint reads that line); the critique follows it.
      html = html.replace(/<style>/, "<style>/* Hallmark: app openbell, shape: app screen, genre: modern-minimal, theme: console (material only, Geist + Geist Mono), critique: one instrument surface, the report read line by line as the audit trail */\n");
      html = html.replace(/--tw-ring-offset-color:\s*#fff\b/g, "--tw-ring-offset-color:var(--surface)");
      html = html.replace(/\.transition\{[^}]*\}/g, "");

      const out = path.resolve(root, "../assets/index.html");
      fs.writeFileSync(out, html);
      const gz = zlib.gzipSync(Buffer.from(html, "utf8"), { level: 9 });
      fs.writeFileSync(out + ".gz", gz);

      const chunksOutDir = path.resolve(root, "../assets/chunks");
      fs.rmSync(chunksOutDir, { recursive: true, force: true });
      fs.mkdirSync(chunksOutDir, { recursive: true });

      const distChunksDir = path.join(dist, "chunks");
      const entries: string[] = [];
      if (fs.existsSync(distChunksDir)) {
        const files = fs.readdirSync(distChunksDir).sort();
        for (const file of files) {
          if (inlinedFiles.has(`chunks/${file}`)) continue;
          const srcPath = path.join(distChunksDir, file);
          if (!fs.statSync(srcPath).isFile()) continue;
          const rawBuf = fs.readFileSync(srcPath);
          if (file.endsWith(".js") && entryJsBasename && rawBuf.toString("utf8").includes(entryJsBasename)) {
            throw new Error(`Lazy chunk ${file} back-imports entry script ${entryJsBasename}`);
          }
          const destRaw = path.join(chunksOutDir, file);
          fs.writeFileSync(destRaw, rawBuf);
          const gzBuf = zlib.gzipSync(rawBuf, { level: 9 });
          fs.writeFileSync(destRaw + ".gz", gzBuf);
          const sha = crypto.createHash("sha256").update(rawBuf).digest("hex");
          entries.push(
            `    EmbeddedChunk { name: ${JSON.stringify(file)}, content_type: ${JSON.stringify(contentTypeFor(file))}, etag: ${JSON.stringify(`"${sha}"`)}, raw: include_bytes!("../assets/chunks/${file}"), gz: include_bytes!("../assets/chunks/${file}.gz") },`,
          );
        }
      }

      const rsOut = path.resolve(root, "../src/embedded_chunks.rs");
      const rsCode =
        "// @generated by frontend/vite.config.ts — do not edit by hand.\n\n" +
        "pub struct EmbeddedChunk {\n" +
        "    pub name: &'static str,\n" +
        "    pub content_type: &'static str,\n" +
        "    pub etag: &'static str,\n" +
        "    pub raw: &'static [u8],\n" +
        "    pub gz: &'static [u8],\n" +
        "}\n\n" +
        "pub static CHUNKS: &[EmbeddedChunk] = &[\n" +
        entries.join("\n") +
        "\n];\n\n" +
        "pub fn lookup(name: &str) -> Option<&'static EmbeddedChunk> {\n" +
        "    CHUNKS.iter().find(|c| c.name == name)\n" +
        "}\n";
      fs.writeFileSync(rsOut, rsCode);
      console.log(
        `single file: ${out} (${(html.length / 1024).toFixed(0)} kB raw, ${(gz.length / 1024).toFixed(0)} kB gzip) + ${entries.length} embedded chunks`,
      );
    },
  };
}

export default defineConfig({
  plugins: [runeUsed(), react(), tailwindcss(), singleFile()],
  base: "/",
  build: {
    target: "es2022",
    assetsInlineLimit: 4096,
    cssCodeSplit: false,
    chunkSizeWarningLimit: 4000,
    modulePreload: false,
    rollupOptions: {
      output: {
        entryFileNames: "chunks/app-[hash].js",
        chunkFileNames: "chunks/[name]-[hash].js",
        assetFileNames: "chunks/[name]-[hash][extname]",
        manualChunks(id) {
          const norm = id.replace(/\\/g, "/");
          if (norm.includes("/node_modules/")) {
            return "vendor";
          }
          if (
            norm.includes("preload-helper") ||
            norm.includes("/rune/used/") ||
            norm.endsWith("/frontend/src/lib/desk.ts") ||
            norm.endsWith("/frontend/src/lib/fees.ts") ||
            norm.endsWith("/frontend/src/lib/lazy.tsx") ||
            norm.endsWith("/frontend/src/lib/live.ts") ||
            norm.endsWith("/frontend/src/lib/media.ts") ||
            norm.endsWith("/frontend/src/TapeCharts.tsx") ||
            norm.endsWith("/frontend/src/AutoCollectorStrip.tsx") ||
            norm.endsWith("/frontend/src/Update.tsx") ||
            norm.endsWith("/frontend/src/motion.ts") ||
            norm.endsWith("/frontend/src/motionProps.ts")
          ) {
            return "shared";
          }
          return undefined;
        },
      },
    },
  },
  server: {
    fs: { allow: [path.resolve(root, "..")] },
    proxy: process.env.API_PORT
      ? {
          "/api": {
            target: `http://127.0.0.1:${process.env.API_PORT}`,
            changeOrigin: true,
            headers: { Origin: `http://127.0.0.1:${process.env.API_PORT}` },
          },
        }
      : undefined,
  },
});
