import { cloudflare } from "@cloudflare/vite-plugin";
import { defineConfig } from "vite";
export default defineConfig({
  plugins: [cloudflare({ remoteBindings: false, tunnel: false,
    persistState: { path: ".cloudflare/state" } })],
  server: { host: "127.0.0.1", port: 8799, strictPort: true },
});
