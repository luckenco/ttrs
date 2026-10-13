import { cloudflare } from "@cloudflare/vite-plugin";
import { defineConfig } from "vite";
import { bindings } from "cf/config";

export default defineConfig({
  plugins: [cloudflare({
    remoteBindings: false,
    tunnel: false,
    persistState: { path: ".cloudflare/state" },
    auxiliaryWorkers: [{ config: {
      name: "read-mcp",
      entrypoint: "./mcp.ts",
      compatibilityDate: "2026-10-06",
      compatibilityFlags: ["nodejs_compat"],
      env: { READ_SERVICE: bindings.worker({ worker: "ttrs-remote-host-lab" }) },
    } }],
  })],
  server: { host: "127.0.0.1", port: 8799, strictPort: true },
});
