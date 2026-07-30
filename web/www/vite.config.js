import { defineConfig } from "vite";
import { viteStaticCopy } from "vite-plugin-static-copy";
import path from "path";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  plugins: [
    viteStaticCopy({
      targets: [
        {
          src: path.resolve(__dirname, "../../../assets/icons"),
          dest: "assets",
        },
      ],
    }),
    {
      name: "serve-assets",
      configureServer(server) {
        server.middlewares.use(
          "/coop/assets",
          (req, res, next) => {
            const assetsPath = path.resolve(__dirname, "../../../assets");
            const filePath = path.join(
              assetsPath,
              req.url.replace("/assets", ""),
            );

            // Try to serve the file
            import("fs").then(({ default: fs }) => {
              if (fs.existsSync(filePath) && fs.statSync(filePath).isFile()) {
                res.setHeader("Access-Control-Allow-Origin", "*");
                if (filePath.endsWith(".svg")) {
                  res.setHeader("Content-Type", "image/svg+xml");
                }
                fs.createReadStream(filePath).pipe(res);
              } else {
                next();
              }
            });
          },
        );
      },
    },
  ],
  build: {
    target: "esnext",
    minify: true,
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks: undefined,
      },
    },
  },
  server: {
    port: 3000,
    open: true,
    fs: {
      strict: false,
      allow: [".."],
    },
    headers: {
      "Cross-Origin-Embedder-Policy": "require-corp",
      "Cross-Origin-Opener-Policy": "same-origin",
    },
  },
  optimizeDeps: {
    exclude: ["./src/wasm"],
  },
  base: "/",
});
