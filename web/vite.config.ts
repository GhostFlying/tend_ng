import react from "@vitejs/plugin-react";
import { env } from "node:process";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: { "/api": env.TEND_URL ?? "http://127.0.0.1:8832" },
  },
});
