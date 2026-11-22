import { useEffect, useState } from "react";
import { docsCopy, type Block } from "./docs-copy";
import { REPO, type Lang } from "./i18n";

function initialLang(): Lang {
  const saved = localStorage.getItem("ruxt-lang");
  if (saved === "ko" || saved === "en") return saved;
  return "en";
}

function homeHref(): string {
  return import.meta.env.BASE_URL;
}

/** Render `code` and `[[label|url]]` inside a string. */
function Rich({ text }: { text: string }) {
  const parts = text.split(/(`[^`]+`|\[\[[^\]]+\]\])/g);
  return (
    <>
      {parts.map((part, i) => {
        if (part.startsWith("`") && part.endsWith("`") && part.length >= 2) {
          return <code key={i}>{part.slice(1, -1)}</code>;
        }
        if (part.startsWith("[[") && part.endsWith("]]")) {
          const inner = part.slice(2, -2);
          const bar = inner.indexOf("|");
          if (bar > 0) {
            return (
              <a key={i} href={inner.slice(bar + 1)}>
                {inner.slice(0, bar)}
              </a>
            );
          }
        }
        return <span key={i}>{part}</span>;
      })}
    </>
  );
}

function BlockView({ block }: { block: Block }) {
  if (block.kind === "p") {
    return (
      <p>
        <Rich text={block.text} />
      </p>
    );
  }
  if (block.kind === "note") {
    return (
      <p className="status-note">
        <Rich text={block.text} />
      </p>
    );
  }
  if (block.kind === "ul") {
    return (
      <ul>
        {block.items.map((item) => (
          <li key={item}>
            <Rich text={item} />
          </li>
        ))}
      </ul>
    );
  }
  return (
    <div className="doc-table-wrap">
      <table className="doc-table">
        <thead>
          <tr>
            {block.headers.map((header) => (
              <th key={header} scope="col">
                {header}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {block.rows.map((row) => (
            <tr key={row.join("\u0001")}>
              {row.map((cell, i) => (
                <td key={i}>
                  <Rich text={cell} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export default function DocsPage() {
  const [lang, setLang] = useState<Lang>(initialLang);
  const [current, setCurrent] = useState("overview");
  const c = docsCopy[lang];

  useEffect(() => {
    document.documentElement.lang = lang;
    localStorage.setItem("ruxt-lang", lang);
    document.title = lang === "en" ? "Ruxt documentation" : "Ruxt 문서";
  }, [lang]);

  useEffect(() => {
    const nodes = c.sections
      .map((section) => document.getElementById(section.id))
      .filter((node): node is HTMLElement => node !== null);
    if (nodes.length === 0) return;
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((entry) => entry.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) setCurrent(visible[0].target.id);
      },
      { rootMargin: "-10% 0px -70% 0px", threshold: [0, 1] },
    );
    for (const node of nodes) observer.observe(node);
    return () => observer.disconnect();
  }, [c.sections]);

  return (
    <>
      <header className="top">
        <a className="wordmark" href={homeHref()} aria-label="Ruxt">
          ru<span>X</span>t
        </a>
        <nav aria-label="Sections">
          <a href={homeHref()}>{c.home}</a>
          <a href={`${homeHref()}#download`}>{c.download}</a>
          <a href={REPO}>GitHub</a>
          <button type="button" className="lang" onClick={() => setLang(lang === "ko" ? "en" : "ko")}>
            {c.langLabel}
          </button>
        </nav>
      </header>

      <main className="doc-page">
        <div className="doc-layout">
          <nav className="doc-toc" aria-label={c.onThisPage}>
            <p className="doc-toc-label">{c.onThisPage}</p>
            {c.sections.map((section) => (
              <a key={section.id} href={`#${section.id}`} aria-current={current === section.id ? "true" : undefined}>
                {section.title}
              </a>
            ))}
          </nav>
          <article>
            <h1>{c.pageTitle}</h1>
            <p className="lede">{c.lede}</p>
            {c.sections.map((section) => (
              <section key={section.id} id={section.id} className="doc-section">
                <h2>{section.title}</h2>
                {section.blocks.map((block, i) => (
                  <BlockView key={i} block={block} />
                ))}
              </section>
            ))}
            <section className="doc-section" id="also">
              <h2>{c.alsoTitle}</h2>
              <ul className="docs">
                {c.also.map((item) => (
                  <li key={item.href}>
                    <a href={item.href}>{item.title}</a>
                  </li>
                ))}
              </ul>
            </section>
          </article>
        </div>
      </main>
    </>
  );
}
