# v0.12.2 frontend validation

This is execution evidence, not a claim that every framework mode is
supported. All successful entries use the same `static/web` adapter and the
same `wasi:http/proxy` runtime contract. Node/pnpm were used only to produce
the static output.

The mandatory React, Vue, Angular, Svelte, Solid, and Preact applications were
built with their normal current Vite/Angular production commands, activated
through PitLane, and requested through PitFast. React, Vue, Angular, and Svelte
also loaded in headless Chromium from PitFast with no fatal page or console
errors. Next.js static export, Nuxt generate, SvelteKit adapter-static, and
Astro static were separately built and executed through the same path.

The machine-readable records, exact versions, output directories, and blocked
SSR classifications are in `v0122-frontend-validation.json`.

The unknown static proof used a project named `mystery-static-framework` with a
prebuilt `dist/` directory and no detector or runtime branch for that name.
