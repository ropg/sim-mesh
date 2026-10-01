import { configure } from 'quasar/wrappers';

// `sim dev` runs `quasar dev` behind the front, which passes the page through
// on its own port. Reached directly instead, the dev server sends /ws, /api
// and /planner to the front here.
const FRONT = process.env.SIM_MESH_FRONT || 'http://127.0.0.1:8800';

export default configure(() => {
  return {
    boot: [],
    css: ['app.css'],
    extras: [],
    build: {
      target: { browser: ['es2022'] },
      vueRouterMode: 'history',
      extendViteConf(viteConf) {
        // planner-wasm finds its .wasm beside its own module; pre-bundled, it
        // would look for it in Vite's cache instead. The page passes the URL
        // explicitly as well, so this only keeps `quasar dev` honest.
        viteConf.optimizeDeps = {
          ...viteConf.optimizeDeps,
          exclude: [...(viteConf.optimizeDeps?.exclude ?? []), 'planner-wasm'],
        };
        if (viteConf.build) {
          viteConf.build.chunkSizeWarningLimit = Infinity;
        }
        // `sim dev` in a container, where edits made outside arrive as no
        // file events: the sources are looked at instead.
        if (process.env.CHOKIDAR_USEPOLLING) {
          viteConf.server = { ...viteConf.server, watch: { usePolling: true, interval: 300 } };
        }
      },
    },
    devServer: {
      open: false,
      // Same-origin with the front, so the websocket, the sidecar and the
      // station links behave exactly as when the front serves the built page.
      proxy: {
        '/ws': { target: FRONT, changeOrigin: true, ws: true },
        '/api': { target: FRONT, changeOrigin: true },
        '/planner': { target: FRONT, changeOrigin: true },
      },
    },
    framework: {
      iconSet: 'svg-material-icons',
      config: { dark: true },
      plugins: ['Dialog', 'Notify'],
    },
  };
});
