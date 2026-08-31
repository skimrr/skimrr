/**
 * Rend les tuiles MSIX en PNG et les écrit directement dans le paquet Windows.
 *
 *   npm run build
 *
 * Pourquoi ce script existe : le bundler (@choochmeque/tauri-windows-bundle) sait
 * recopier StoreLogo / Square44x44 / Square150x150 depuis src-tauri/icons, mais il
 * n'a aucune source pour la tuile large 310x150 — il tente de la fabriquer en
 * collant l'icône carrée sur un canevas, ce qui a produit un rectangle noir uni.
 * C'est ce visuel par défaut qui a fait échouer la certification Microsoft Store
 * (10.1.1.11 On Device Tiles) sur skimrr_0.3.0.0.msixbundle.
 *
 * Le rendu passe par Chrome plutôt que par une bibliothèque d'images : c'est le seul
 * moyen simple d'obtenir la vraie police de marque, livrée en woff2 (assets/).
 */
import puppeteer from "puppeteer-core";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const ASSETS = join(here, "../../src-tauri/gen/windows/Assets");

const CHROME =
  process.env.CHROME_PATH ??
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/** Les dimensions sont celles du manifeste : les changer casserait le paquet. */
const TILES = [{ source: "wide310x150.html", out: "Wide310x150Logo.png", width: 310, height: 150 }];

const browser = await puppeteer.launch({
  executablePath: CHROME,
  headless: true,
  args: ["--hide-scrollbars", "--disable-gpu", "--force-color-profile=srgb"],
});

try {
  const page = await browser.newPage();

  for (const tile of TILES) {
    await page.setViewport({
      width: tile.width,
      height: tile.height,
      deviceScaleFactor: 1,
    });
    await page.goto(pathToFileURL(join(here, tile.source)).href, {
      waitUntil: "networkidle0",
    });
    // Sans cette attente la première capture sort en police système.
    await page.evaluate(() => document.fonts.ready);

    // Le mot dicte la largeur de la barre ambre : on la relève pour vérifier que
    // le verrou tient dans la tuile au lieu de le supposer.
    const lockup = await page.evaluate(() => {
      const box = document.querySelector(".lockup").getBoundingClientRect();
      return { width: Math.round(box.width), height: Math.round(box.height) };
    });
    console.log(`  ${tile.out}: verrou ${lockup.width}x${lockup.height} px`);

    // omitBackground conserve la transparence hors du rayon des coins : sans lui
    // Chrome peint un fond blanc et les angles ne sont plus transparents.
    await page.screenshot({
      path: join(ASSETS, tile.out),
      type: "png",
      omitBackground: true,
      captureBeyondViewport: false,
    });
  }
} finally {
  await browser.close();
}

console.log(`${TILES.length} tuile(s) écrite(s) dans ${ASSETS}`);
