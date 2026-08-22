import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import fr from "./locales/fr.json";
import es from "./locales/es.json";
import de from "./locales/de.json";
import ja from "./locales/ja.json";
import zh from "./locales/zh.json";

export const LANGUAGES = [
  { code: "en", label: "English" },
  { code: "fr", label: "Français" },
  { code: "es", label: "Español" },
  { code: "de", label: "Deutsch" },
  { code: "ja", label: "日本語" },
  { code: "zh", label: "简体中文" },
] as const;

const saved = localStorage.getItem("skimrr-lang");
const nav = navigator.language.toLowerCase();
const detected =
  saved ??
  LANGUAGES.map((l) => l.code).find((code) => nav.startsWith(code)) ??
  "en";

i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    fr: { translation: fr },
    es: { translation: es },
    de: { translation: de },
    ja: { translation: ja },
    zh: { translation: zh },
  },
  lng: detected,
  fallbackLng: "en",
  interpolation: { escapeValue: false },
});

i18n.on("languageChanged", (lng) => {
  document.documentElement.lang = lng;
  localStorage.setItem("skimrr-lang", lng);
});
document.documentElement.lang = i18n.language;

export default i18n;
