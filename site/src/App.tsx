import { useEffect, useState } from "react";
import Download from "./Download";
import Terminal from "./Terminal";
import { copy, Lang, REPO } from "./i18n";

function initialLang(): Lang {
  const saved = localStorage.getItem("ruxt-lang");
  if (saved === "ko" || saved === "en") return saved;
  return navigator.language.toLowerCase().startsWith("ko") ? "ko" : "en";
}

export default function App() {
  const [lang, setLang] = useState<Lang>(initialLang);
  const c = copy[lang];

  useEffect(() => {
    document.documentElement.lang = lang;
    localStorage.setItem("ruxt-lang", lang);
    document.title = `Ruxt — ${c.tagline}`;
  }, [lang, c.tagline]);

  return (
    <>
      <header className="top">
        <a className="wordmark" href="#top" aria-label="Ruxt">
          ru<span>X</span>t
        </a>
        <nav aria-label="Sections">
          <a href="#defends">{c.defendsTitle}</a>
          <a href="#features">{c.featuresTitle}</a>
          <a href="#download">{c.downloadTitle}</a>
          <a href={`${import.meta.env.BASE_URL}docs.html`}>{c.docsTitle}</a>
          <a href={REPO}>GitHub</a>
          <button type="button" className="lang" onClick={() => setLang(lang === "ko" ? "en" : "ko")}>
            {c.langLabel}
          </button>
        </nav>
      </header>

      <main id="top">
        <section className="hero">
          <div className="hero-text">
            <h1>{c.tagline}</h1>
            <p className="lede">{c.lede}</p>
            <p className="cta">
              <a className="btn btn-primary" href="#download">
                {c.ctaDownload}
              </a>
              <a className="btn" href={REPO}>
                {c.ctaSource}
              </a>
            </p>
            <p className="status-note">{c.statusNote}</p>
          </div>
          <div className="hero-term">
            <Terminal title={c.terminalTitle} />
            <p className="caption">{c.terminalCaption}</p>
          </div>
        </section>

        <section id="defends" className="section">
          <h2>{c.defendsTitle}</h2>
          <p className="intro">{c.defendsIntro}</p>
          <dl className="threats">
            {c.rows.map((r) => (
              <div className="threat" key={r.threat}>
                <dt>{r.threat}</dt>
                <dd>{r.defense}</dd>
              </div>
            ))}
          </dl>
        </section>

        <section id="features" className="section">
          <h2>{c.featuresTitle}</h2>
          <div className="groups">
            {c.groups.map((g) => (
              <div className="group" key={g.title}>
                <h3>{g.title}</h3>
                <ul>
                  {g.items.map((it) => (
                    <li key={it}>{it}</li>
                  ))}
                </ul>
              </div>
            ))}
          </div>
        </section>

        <section id="screens" className="section">
          <h2>{c.screensTitle}</h2>
          <div className="shots">
            <figure className="shot">
              <img src="images/sftp-explorer.png" alt={c.screenExplorer} width={980} height={420} />
              <figcaption>{c.screenExplorer}</figcaption>
            </figure>
          </div>
        </section>

        <Download c={c} lang={lang} />

        <section id="docs" className="section">
          <h2>{c.docsTitle}</h2>
          <ul className="docs">
            {c.docs.map((d) => (
              <li key={d.href}>
                <a href={d.href}>{d.title}</a>
                <span>{d.desc}</span>
              </li>
            ))}
          </ul>
        </section>
      </main>

      <footer className="foot">
        <p>Ruxt · {c.footerLicense}</p>
        <p>{c.footerBundle}</p>
        <p>{c.footerHonest}</p>
      </footer>
    </>
  );
}
