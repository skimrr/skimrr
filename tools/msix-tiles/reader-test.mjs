import puppeteer from "puppeteer-core";
const [, , base, plain, locked, password, shots] = process.argv;
const browser = await puppeteer.launch({
  executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  headless: true,
});
const page = await browser.newPage();
await page.setViewport({ width: 1280, height: 1000, deviceScaleFactor: 2 });

const net = [];
page.on("request", (r) => net.push(r.url()));
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (m) => m.type() === "error" && errors.push(m.text()));

const ok = (label, cond, detail = "") =>
  console.log(`${cond ? "  ok " : "FAIL"} ${label}${detail ? ` — ${detail}` : ""}`);

await page.goto(`${base}/open/`, { waitUntil: "networkidle0" });
ok("la page se charge", await page.$("#drop") !== null);
// Rien ne doit être visible tant qu'aucun fichier n'est choisi : `hidden` doit vraiment
// cacher, ce que `display: grid` défaisait.
const visibleBefore = await page.$$eval("#facts, #project", (n) =>
  n.filter((e) => e.getBoundingClientRect().height > 0).length);
ok("rien d'ouvert n'est affiché au départ", visibleBefore === 0, `${visibleBefore} bloc(s) visible(s)`);

// --- conteneur non chiffré
await (await page.$("#file")).uploadFile(plain);
await page.waitForFunction(() => !document.getElementById("facts").hidden, { timeout: 15000 });
const facts = await page.$eval("#factList", (n) => n.innerText.replace(/\n/g, " · "));
ok("l'en-tête est lu sans clé", facts.length > 0, facts);
ok("aucun champ mot de passe", await page.$eval("#unlock", (n) => n.hidden));

await page.click("#open");
await page.waitForFunction(() => !document.getElementById("project").hidden, { timeout: 20000 });
const name = await page.$eval("#projectName", (n) => n.textContent);
const meta = await page.$eval("#projectMeta", (n) => n.textContent);
const thumbs = await page.$$eval("#grid img", (n) => n.length);
const rows = await page.$$eval("#rows tr", (n) => n.length);
ok("le projet s'ouvre", name.length > 0, `${name} — ${meta}`);
ok("les vignettes s'affichent", thumbs > 0, `${thumbs} images`);
// Une vignette qui ne se décode pas laisse naturalWidth à zéro : compter les <img> ne
// prouve rien, il faut que le navigateur ait vraiment lu les octets.
await new Promise((r) => setTimeout(r, 800));
const decoded = await page.$$eval("#grid img", (n) => n.filter((i) => i.naturalWidth > 0).length);
ok("les vignettes se décodent", decoded === thumbs, `${decoded}/${thumbs} images rendues`);
ok("le tableau se remplit", rows > 0, `${rows} lignes`);
await page.screenshot({ path: `${shots}/reader-plain.png` });

// --- conteneur chiffré : mauvais puis bon mot de passe
await page.click("#reset");
await (await page.$("#file")).uploadFile(locked);
await page.waitForFunction(() => !document.getElementById("facts").hidden, { timeout: 15000 });
ok("le chiffrement est annoncé sans clé", !(await page.$eval("#unlock", (n) => n.hidden)),
   (await page.$eval("#factList", (n) => n.innerText)).replace(/\n/g, " · "));

await page.type("#password", "pas le bon");
await page.click("#open");
await page.waitForFunction(() => !document.getElementById("error").hidden, { timeout: 30000 });
ok("un mauvais mot de passe est refusé", true, await page.$eval("#error", (n) => n.textContent));

await page.type("#password", password);
await page.click("#open");
await page.waitForFunction(() => !document.getElementById("project").hidden, { timeout: 30000 });
ok("le bon mot de passe ouvre", (await page.$eval("#projectName", (n) => n.textContent)).length > 0);
ok("le champ est vidé après usage", (await page.$eval("#password", (n) => n.value)) === "");
await page.screenshot({ path: `${shots}/reader-locked.png` });

// --- fermeture
await page.click("#close");
ok("fermer vide la grille", (await page.$$eval("#grid img", (n) => n.length)) === 0);

// --- ce qui a réellement traversé le réseau
// `blob:` et `data:` sont des URL locales : elles désignent de la mémoire, pas une
// destination réseau. Ce qui compte est ce qui sort du domaine.
const outbound = net.filter(
  (u) => !u.startsWith(base) && !u.startsWith("data:") && !u.startsWith("blob:"),
);
ok("rien n'est sorti du domaine", outbound.length === 0, outbound.join(" ") || "aucune requête externe");
ok("aucune erreur console", errors.length === 0, errors.slice(0, 2).join(" | "));
console.log("\n  requêtes réseau :", [...new Set(net.filter((u) => !u.startsWith("blob:")).map((u) => u.replace(base, "")))].join(" "));
await browser.close();
