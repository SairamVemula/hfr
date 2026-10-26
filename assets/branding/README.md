# HFR brand kit

**HFR** = **H**yper**f**rames **R**ender. The mark is an "H" built from two film strips with a play button between them: Hyperframes, rendered elsewhere.

Everything here is generated from one glyph definition by `build.mjs`, so the variants never drift apart:

```bash
cd assets/branding
npm install
node build.mjs
```

## Files

| File | Use |
| --- | --- |
| `icon.svg`, `icon-*.png`, `icon.ico`, `icon.icns` | The app icon (white mark on the violet gradient). Used by the Windows/macOS/Linux packages and the executables |
| `mark.svg` | Gradient mark without a tile: README headers, documentation |
| `variants/icon-dark.svg` | Dark tile, gradient mark; sits well next to the dark app |
| `variants/icon-light.svg` | Light tile, for light backgrounds and documents |
| `variants/icon-glow.svg` | Dark tile with a violet bloom: hero and store artwork |
| `variants/icon-circle.svg` | Round avatar (GitHub organisation, social profiles) |
| `variants/mark-white.svg`, `mark-black.svg` | One colour, no tile: tray and menu-bar icons, favicons, print |
| `variants/icon-ember`, `icon-mint`, `icon-ocean` | Alternative colourways, for example to mark pre-release builds or a themed event |
| `variants/logo-horizontal.svg`, `logo-horizontal-dark.svg` | Icon + "HFR" wordmark, for light and dark backgrounds (letters are drawn as shapes, no font needed) |
| `social-preview.png` | 1280 x 640 image for the repository (GitHub: Settings > General > Social preview) |
| `variants.png` | Contact sheet of everything, including 16-48 px renderings |

## Rules

- Violet gradient `#9B7BFF` to `#5B4CF0` is the brand colour; text on dark uses `#A992FF` (4.5:1 contrast).
- Keep clear space of at least one film-strip width around the mark. Do not stretch, rotate, outline or recolour it outside the colourways above.
- Below 24 px use the tile version (`icon.svg`); below 16 px only the strips and triangle remain readable, which is intended.
- Name in text: **HFR** (the CLI is `hfr`, the Rust crates are `hfr-*`). Tagline: *Hyperframes Render*.
