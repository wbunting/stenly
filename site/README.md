# Stenly marketing site

A standalone, single-page **Zola + Tailwind CSS v4** site. No changes to the Rust app, no runtime framework, analytics, CDN scripts, or remote font requests.

## Develop

Install [Zola](https://www.getzola.org/documentation/getting-started/installation/) (tested with 0.23.6) and Node.js 22+.

```sh
cd site
npm ci
npm run dev
# http://127.0.0.1:1111
```

The dev command builds CSS first, then runs the Tailwind watcher and Zola's live server together. For a non-PATH Zola binary: `ZOLA_BIN=/path/to/zola npm run dev`. For a different port: `PORT=1234 npm run dev`.

## Build / deploy

```sh
npm run build
npm run check
```

Publish `site/public/` to any static host. For production deployment to Cloudflare Pages, the build uses relative asset and navigation URLs so previews work too, while keeping Zola's production base URL for canonical metadata and the sitemap:

```sh
cd site
npm ci
npm run deploy:pages
```

The deploy script uses the locally pinned Wrangler CLI (no global CLI install needed).

This keeps CSS, JS, fonts, icons, and internal links same-origin on both `*.pages.dev` and `stenly.ai`. Canonical metadata and sitemap URLs continue to use `config.toml` (`https://stenly.ai`). For a different deployment URL, use `npm run css && zola build --base-url https://example.com`. CSS and JS URLs are served from stable same-origin paths. Generated output and compiled CSS are ignored; build before deploying. No application backend is needed.

### GitHub Actions deployment

`.github/workflows/site-deploy.yml` tests, checks, builds, and deploys the exact triggering `main` commit to the production Cloudflare Pages project when site files or the workflow change. It also supports manual `workflow_dispatch` runs against `main`; dispatching from another ref intentionally skips deployment. Pull requests run the separate non-deploy site CI job.

Configure repository settings before the first Actions deployment:

- Add the Cloudflare Pages API token as the Actions secret `CLOUDFLARE_API_TOKEN`. Use a dedicated token with only the account-scoped Cloudflare Pages edit permission needed for Pages deployments; do not reuse a personal OAuth token.
- Add the Pages account ID as the Actions variable `CLOUDFLARE_ACCOUNT_ID` (`0fb0c931c18bd143d818adbe246bd105` for the current `stenly-site` project).
- Keep the Pages project name `stenly-site` and production branch `main` in sync with the workflow.

A successful local deployment does not prove the Actions credential has been configured: confirm a successful workflow run after adding the secret and variable.

## Downloads and hosting

Edit `config.toml`:

- `extra.macos_download_url`: real macOS release asset URL.
- `extra.linux_download_url`: future Linux release asset URL; not linked while the asset does not exist.
- `extra.linux_install_options`: optional array of `{ name, url }` distro instruction links. By default it points to the built-in Ubuntu / Debian, Fedora, and Arch pages.

Home page downloads: the **Download for macOS** button links straight to `extra.macos_download_url` (it works without JavaScript; JS only adds the Apple icon on Macs). Linux has no release asset yet, so the home page shows **Linux: coming soon** with links to the pre-release setup guides, and `extra.linux_download_url` is deliberately not linked anywhere (tests enforce this). When a Linux asset ships, link it and update the tests. `extra.release_notes_url` and `extra.macos_min_version` feed the macOS details line. Icons are self-hosted Simple Icons (CC0; see `static/icons/LICENSE.md`).
- `extra.license_price`, `extra.free_meetings`, and `extra.team_contact_url`: values used by `/pricing/`. Defaults are $10, 10 free meetings, and a team mailto placeholder.
- `extra.hosted_url`: optional external hosted-service URL.
- `extra.repository_url`: source repository and setup instructions.

“Open source” is simply a GitHub link. Download buttons use stable GitHub release asset names: `.github/workflows/release.yml` uploads `stenly-macos.dmg` and `stenly-linux.tar.gz` when a `v*` tag is pushed. No private-repository notices or source-build detours are shown. The optional hosted link is absent until configured.

## Edit

- `templates/index.html`: copy, page structure, synthetic meeting example.
- `assets/site.css`: Tailwind entry and editorial design.
- `static/site.js`: Notes/Transcript switching and download-picker enhancement.
- `static/fonts/`: locally served OFL fonts, with license files.
- `DESIGN.md` and `PRODUCT.md`: direction and product-claim boundaries.

The meeting example is fictional and labeled as such. Local-first is not a blanket promise that no data can leave: users can opt into external model endpoints and cloud destinations.

## Browser tests

```sh
npm exec playwright install chromium
npm test
```

Tests start their own server on port 1112. They cover desktop fit, mobile overflow, example switching, the macOS download link, the Linux coming-soon note, no-JavaScript access, no Linux tarball links, no price or licence claims, and missing asset requests. Evidence stays in ignored directories, not Git.
