// Generates every logo file from one glyph definition, so the variants always agree.
//
//   cd assets/branding && npm install && node build.mjs
//
// Outputs: icon.svg and its PNG/ICO/ICNS set (the app icon), variants/*.svg + png, wordmark lockups,
// the GitHub social preview, and a contact sheet (variants.png) to compare everything at a glance.
// Needs Node 18+; the only dependencies are @resvg/resvg-js (SVG -> PNG) and png2icons (ICO/ICNS).

import { mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Resvg } from "@resvg/resvg-js";
import png2icons from "png2icons";

const here = dirname(fileURLToPath(import.meta.url));
const out = (p) => join(here, p);
mkdirSync(out("variants"), { recursive: true });

// ---- palette -----------------------------------------------------------------------------------------------
const VIOLET = ["#9B7BFF", "#5B4CF0"]; // brand
const INK = "#0B0D14";
const colorways = {
  violet: VIOLET,
  ember: ["#FF9A5C", "#E5484D"],
  mint: ["#3DDBA4", "#0E9AA7"],
  ocean: ["#4CC2FF", "#2D5BEA"],
};

// ---- the glyph ---------------------------------------------------------------------------------------------
// An "H" made of two film strips (the bars, with perforations) with a play button between them: Hyperframes.
// Coordinates are in a 512 x 512 box. `id` keeps the mask unique when several glyphs share one document.
function glyph(fill, id = "g") {
  const holes = [0, 1, 2, 3, 4].flatMap((i) => [
    `<rect x="128" y="${124 + 60 * i}" width="24" height="24" rx="7" fill="#000"/>`,
    `<rect x="360" y="${124 + 60 * i}" width="24" height="24" rx="7" fill="#000"/>`,
  ]);
  return `
  <mask id="${id}-cut" maskUnits="userSpaceOnUse" x="0" y="0" width="512" height="512">
    <rect width="512" height="512" fill="#fff"/>
    ${holes.join("\n    ")}
  </mask>
  <g mask="url(#${id}-cut)" fill="${fill}">
    <rect x="104" y="96" width="72" height="320" rx="20"/>
    <rect x="336" y="96" width="72" height="320" rx="20"/>
  </g>
  <path d="M216 186V326L294 256Z" fill="${fill}" stroke="${fill}" stroke-width="20" stroke-linejoin="round"/>`;
}

const grad = (id, [a, b]) =>
  `<linearGradient id="${id}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient>`;
// Same colours, but anchored to the 512 box so a glyph made of several shapes gets one continuous gradient.
const gradGlyph = (id, [a, b]) =>
  `<linearGradient id="${id}" gradientUnits="userSpaceOnUse" x1="96" y1="96" x2="416" y2="416"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient>`;

const svg = (body, defs = "", size = 512) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" width="${size}" height="${size}">${defs ? `<defs>${defs}</defs>` : ""}${body}\n</svg>\n`;

// ---- icon variants (all 512 x 512) --------------------------------------------------------------------------
const tile = (fill, extra = "") => `<rect width="512" height="512" rx="112" fill="${fill}"/>${extra}`;
const files = {};

// 1. Main app icon: white glyph on the violet gradient.
files["icon"] = svg(`${tile("url(#bg)")}${glyph("#FFFFFF", "i")}`, grad("bg", VIOLET));

// 2. Dark tile, gradient glyph (looks best next to the dark app UI).
files["icon-dark"] = svg(
  `${tile(INK)}<rect x="6" y="6" width="500" height="500" rx="106" fill="none" stroke="#2A2F4A" stroke-width="3"/>${glyph("url(#gl)", "d")}`,
  gradGlyph("gl", VIOLET),
);

// 3. Light tile, gradient glyph (documents, light backgrounds).
files["icon-light"] = svg(
  `${tile("#F3F0FF")}<rect x="6" y="6" width="500" height="500" rx="106" fill="none" stroke="#DDD6FF" stroke-width="3"/>${glyph("url(#gl)", "l")}`,
  gradGlyph("gl", ["#8B6CFF", "#4F3FE0"]),
);

// 4. Glow: dark tile, a soft violet bloom behind a white glyph (hero and store art).
files["icon-glow"] = svg(
  `${tile(INK)}<circle cx="256" cy="256" r="230" fill="url(#halo)"/>${glyph("#FFFFFF", "w")}`,
  `<radialGradient id="halo" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#7B5CFF" stop-opacity="0.75"/><stop offset="1" stop-color="#7B5CFF" stop-opacity="0"/></radialGradient>`,
);

// 5. Circle avatar (GitHub organisation / social profile).
files["icon-circle"] = svg(`<circle cx="256" cy="256" r="256" fill="url(#bg)"/><g transform="translate(256 256) scale(0.88) translate(-256 -256)">${glyph("#FFFFFF", "c")}</g>`, grad("bg", VIOLET));

// 6. Marks without a tile: gradient, white, black (README headers, favicons, tray icons, single-colour print).
files["mark"] = svg(glyph("url(#gl)", "m"), gradGlyph("gl", VIOLET));
files["mark-white"] = svg(glyph("#FFFFFF", "mw"));
files["mark-black"] = svg(glyph("#0B0D14", "mb"));

// 7. Colourways of the main icon, to pick from or to mark pre-release builds.
for (const [name, c] of Object.entries(colorways)) {
  if (name !== "violet") files[`icon-${name}`] = svg(`${tile("url(#bg)")}${glyph("#FFFFFF", "x")}`, grad("bg", c));
}

// ---- lockups: glyph + the letters HFR drawn as strokes (no font needed) -----------------------------------------
function letters(color, h = 200, w = 46) {
  // Each letter is 130 wide, 200 tall, drawn with a square-capped stroke of width `w`.
  const s = `stroke="${color}" stroke-width="${w}" fill="none" stroke-linejoin="miter" stroke-linecap="butt"`;
  const H = `<path ${s} d="M23 0V${h} M107 0V${h} M23 ${h / 2}H107"/>`;
  const F = `<path ${s} d="M23 0V${h} M0 23H118 M23 ${h / 2 - 4}H100"/>`;
  const R = `<path ${s} d="M23 ${h}V0 M0 23H72A42.5 42.5 0 0 1 72 108H23"/><path d="M62 98H108L134 ${h}H88Z" fill="${color}"/>`;
  return `<g transform="translate(0 0)">${H}</g><g transform="translate(170 0)">${F}</g><g transform="translate(330 0)">${R}</g>`;
}

function lockup(theme) {
  const dark = theme === "dark";
  const word = dark ? "#FFFFFF" : "#14122B";
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1180 512" width="1180" height="512">
  <defs>${grad("bg", VIOLET)}</defs>
  <rect x="0" y="0" width="512" height="512" rx="112" fill="url(#bg)"/>
  ${glyph("#FFFFFF", "k")}
  <g transform="translate(600 156) scale(1.0)">${letters(word)}</g>
  <rect x="604" y="400" width="440" height="6" rx="3" fill="url(#bg)"/>
</svg>\n`;
}
files["logo-horizontal"] = lockup("light");
files["logo-horizontal-dark"] = lockup("dark");

// ---- GitHub social preview (1280 x 640) -------------------------------------------------------------------------
const social = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1280 640" width="1280" height="640">
  <defs>
    ${grad("bg", VIOLET)}
    <linearGradient id="sky" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#0E1020"/><stop offset="1" stop-color="#171436"/></linearGradient>
    <radialGradient id="halo" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#6D4DFF" stop-opacity="0.55"/><stop offset="1" stop-color="#6D4DFF" stop-opacity="0"/></radialGradient>
  </defs>
  <rect width="1280" height="640" fill="url(#sky)"/>
  <circle cx="1010" cy="320" r="430" fill="url(#halo)"/>
  <g transform="translate(110 96)">
    <g transform="scale(0.25)"><rect width="512" height="512" rx="112" fill="url(#bg)"/>${glyph("#FFFFFF", "s")}</g>
    <g transform="translate(160 32) scale(0.32)">${letters("#FFFFFF")}</g>
  </g>
  <text x="110" y="330" font-family="Segoe UI, Inter, Arial, sans-serif" font-weight="700" font-size="64" fill="#FFFFFF">Render Hyperframes</text>
  <text x="110" y="405" font-family="Segoe UI, Inter, Arial, sans-serif" font-weight="700" font-size="64" fill="#B9A6FF">on your own VPS.</text>
  <text x="110" y="490" font-family="Segoe UI, Inter, Arial, sans-serif" font-size="27" fill="#9AA3BD">Upload once  ·  render with any options  ·  download verified</text>
  <g transform="translate(850 136) scale(0.72)"><rect width="512" height="512" rx="112" fill="url(#bg)"/>${glyph("#FFFFFF", "sh")}</g>
</svg>\n`;
files["social-preview"] = social;

// ---- write the SVGs ----------------------------------------------------------------------------------------------
const place = (name) => (name === "icon" ? out("icon.svg") : name === "mark" ? out("mark.svg") : out(`variants/${name}.svg`));
for (const [name, content] of Object.entries(files)) writeFileSync(place(name), content);

// ---- rasterise ---------------------------------------------------------------------------------------------------
const fonts = ["C:/Windows/Fonts/segoeui.ttf", "C:/Windows/Fonts/segoeuib.ttf"].filter(existsSync);
function png(svgText, width) {
  const r = new Resvg(svgText, { fitTo: { mode: "width", value: width }, font: { fontFiles: fonts, loadSystemFonts: true, defaultFontFamily: "Segoe UI" } });
  return r.render().asPng();
}

// App icon set.
for (const s of [16, 32, 48, 64, 128, 256, 512, 1024]) writeFileSync(out(`icon-${s}.png`), png(files.icon, s));
writeFileSync(out("mark-128.png"), png(files.mark, 128));
const big = readFileSync(out("icon-1024.png"));
writeFileSync(out("icon.ico"), png2icons.createICO(big, png2icons.BEZIER, 0, true, true));
writeFileSync(out("icon.icns"), png2icons.createICNS(big, png2icons.BEZIER, 0));

// A PNG of every variant (for READMEs and quick previews).
for (const name of Object.keys(files)) {
  if (name === "icon" || name === "social-preview" || name.startsWith("logo")) continue;
  writeFileSync(out(`variants/${name}.png`), png(files[name], 512));
}
for (const name of ["logo-horizontal", "logo-horizontal-dark"]) writeFileSync(out(`variants/${name}.png`), png(files[name], 1180));
writeFileSync(out("social-preview.png"), png(files["social-preview"], 1280));

// ---- contact sheet -----------------------------------------------------------------------------------------------
const names = ["icon", "icon-dark", "icon-light", "icon-glow", "icon-circle", "icon-ember", "icon-mint", "icon-ocean"];
const cell = 200, pad = 28, cols = 4;
const inner = (name) => (name === "icon" ? files.icon : files[name]).replace(/<svg[^>]*>/, "").replace(/<\/svg>\s*$/, "");
const tiles = names
  .map((n, i) => {
    const x = pad + (i % cols) * (cell + pad), y = pad + Math.floor(i / cols) * (cell + pad + 26);
    const uid = n.replace(/[^a-z]/g, "");
    // Give every embedded document its own ids so gradients and masks do not collide.
    const body = inner(n).replace(/id="([^"]+)"/g, `id="${uid}-$1"`).replace(/url\(#([^)]+)\)/g, `url(#${uid}-$1)`).replace(/mask="url\(#[^)]+\)"/g, (m) => m);
    return `<g transform="translate(${x} ${y}) scale(${cell / 512})">${body}</g><text x="${x}" y="${y + cell + 20}" font-family="Segoe UI, Arial" font-size="15" fill="#9AA3BD">${n}</text>`;
  })
  .join("\n");
const strip = [16, 24, 32, 48].map((s, i) => `<g transform="translate(${pad + i * 80} ${2 * (cell + pad + 26) + pad}) scale(${s / 512})">${inner("icon").replace(/id="([^"]+)"/g, `id="s${i}-$1"`).replace(/url\(#([^)]+)\)/g, `url(#s${i}-$1)`)}</g>`).join("\n");
const W = pad + cols * (cell + pad), H = 2 * (cell + pad + 26) + 120;
const sheet = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}"><rect width="${W}" height="${H}" fill="#0B0D14"/>${tiles}${strip}<text x="${pad + 4 * 80}" y="${2 * (cell + pad + 26) + pad + 30}" font-family="Segoe UI, Arial" font-size="15" fill="#9AA3BD">small sizes: 16 / 24 / 32 / 48 px</text></svg>`;
writeFileSync(out("variants.png"), png(sheet, W * 2));
console.log("done:", Object.keys(files).length, "svg files");
