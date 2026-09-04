import puppeteer from "puppeteer-core";
const browser = await puppeteer.launch({
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  headless: true });
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 2 });
await page.goto(process.argv[2], { waitUntil: "networkidle0" });
// Les sections apparaissent au défilement ; on les force visibles pour la capture.
await page.addStyleTag({ content: ".reveal{opacity:1!important;transform:none!important}" });
await page.$eval("#projects", (n) => n.scrollIntoView());
await new Promise((r) => setTimeout(r, 700));
// Un délai négatif fait sauter l'animation à un instant précis de sa boucle.
for (const t of [1.0, 2.0, 3.4, 4.6]) {
  await page.$$eval(
    ".vault-panel, .vault-lock, .vault-shackle, .vault-dots i, .vault-grid span",
    (els, t) => els.forEach((e) => {
      const extra = parseFloat(getComputedStyle(e).getPropertyValue("--i") || "0");
      e.style.animationDelay = `-${t}s`;
      e.style.animationPlayState = "paused";
    }), t);
  await new Promise((r) => setTimeout(r, 300));
  await (await page.$(".vault")).screenshot({ path: `${process.argv[3]}/vault-${t}.png` });
}
await browser.close();
