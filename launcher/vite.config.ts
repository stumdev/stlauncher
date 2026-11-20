import { defineConfig } from "vite";

// Porta fixa para o `tauri dev`; build enxuto (sem sourcemaps) para o .exe ficar pequeno.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: { target: "es2022", sourcemap: false, chunkSizeWarningLimit: 600 },
});
