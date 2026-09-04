import puppeteer from "puppeteer-core";
const browser = await puppeteer.launch({
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  headless: true,
});
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 1150, deviceScaleFactor: 2 });
await page.goto(process.argv[2], { waitUntil: "networkidle0" });
await page.screenshot({ path: process.argv[3] });
await browser.close();
