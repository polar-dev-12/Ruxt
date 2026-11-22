import { useEffect, useState } from "react";
import { statusLine, transcript } from "./i18n";

/**
 * The hero: a Ruxt window replaying a real session. Lines appear one after another once
 * (the page's single motion moment); with `prefers-reduced-motion` everything is shown at
 * once.
 */
export default function Terminal({ title }: { title: string }) {
  const reduced =
    typeof window !== "undefined" && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const [shown, setShown] = useState(reduced ? transcript.length : 0);

  useEffect(() => {
    if (reduced || shown >= transcript.length) return;
    const t = window.setTimeout(() => setShown((n) => n + 1), transcript[shown].delay);
    return () => window.clearTimeout(t);
  }, [shown, reduced]);

  const done = shown >= transcript.length;

  return (
    <figure className="term" aria-label="Ruxt session transcript">
      <div className="term-bar">
        <span className="term-dot" />
        <span className="term-title">{title}</span>
        <span className="term-tabs">
          <span className="term-tab term-tab-on">db-01</span>
          <span className="term-tab">bastion</span>
          <span className="term-tab">Local</span>
        </span>
      </div>
      <pre className="term-body">
        {transcript.slice(0, shown).map((l, i) => (
          <span key={i} className={`term-line term-${l.kind}`}>
            {l.text}
            {"\n"}
          </span>
        ))}
        <span className={`term-cursor${done ? " term-cursor-blink" : ""}`} aria-hidden="true" />
      </pre>
      <div className="term-status">{statusLine}</div>
    </figure>
  );
}
