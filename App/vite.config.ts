import { defineConfig, lazyPlugins } from "vite-plus"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"
export default defineConfig({
  resolve: {
    // react-dialog ships a motion object created by its own react-motion copy;
    // a second bundled copy loses the module-local PRESENCE symbol and crashes
    // every dialog on open. Force a single instance.
    dedupe: ["@fluentui/react-motion"],
  },
  fmt: {
    semi: false,
  },
  lint: {
    jsPlugins: [{ name: "vite-plus", specifier: "vite-plus/oxlint-plugin" }],
    rules: { "vite-plus/prefer-vite-plus-imports": "error" },
    options: { typeAware: true, typeCheck: true },
  },
  plugins: lazyPlugins(() => [tailwindcss(), react()]),
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // Rust owns these files; watching loaded DLLs fails with EBUSY on Windows.
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "es2022", reportCompressedSize: false },
})
