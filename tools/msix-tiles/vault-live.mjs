import puppeteer from "puppeteer-core";
const browser = await puppeteer.launch({
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  headless: true });
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 900, deviceScaleFactor: 2 });
await page.goto(process.argv[2], { waitUntil: "networkidle0" });
await page.addStyleTag({ content: ".reveal{opacity:1!important;transform:none!important}" });
await page.$eval("#projects", (n) => n.scrollIntoView());
// On repart d'une boucle propre : les animations sont relancées ensemble.
await page.$$eval(".vault *", (els) => els.forEach((e) => {
  e.style.animation = "none"; void e.offsetWidth; e.style.animation = "";
}));
const t0 = Date.now();
const el = await page.$(".vault");
for (const at of [700, 1600, 2600, 3400, 4600, 6400]) {
  await new Promise((r) => setTimeout(r, Math.max(0, at - (Date.now() - t0))));
  await el.screenshot({ path: `${process.argv[3]}/live-${at}.png` });
}
await browser.close();
