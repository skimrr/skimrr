import puppeteer from "puppeteer-core";
const browser = await puppeteer.launch({
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  headless: true });
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 1000, deviceScaleFactor: 2 });
await page.goto(process.argv[2], { waitUntil: "networkidle0" });
await page.addStyleTag({ content: ".reveal{opacity:1!important;transform:none!important}" });
const el = await page.$(process.argv[3]);
await el.scrollIntoView();
await new Promise((r) => setTimeout(r, parseInt(process.argv[5] || "4000", 10)));
await el.screenshot({ path: process.argv[4] });
await browser.close();
