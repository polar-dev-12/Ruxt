import { useEffect, useState } from "react";
import { Copy, REPO } from "./i18n";

interface Asset {
  name: string;
  browser_download_url: string;
  size: number;
}
interface Release {
  tag_name: string;
  html_url: string;
  published_at: string;
  prerelease: boolean;
  assets: Asset[];
}

const API = REPO.replace("https://github.com/", "https://api.github.com/repos/") + "/releases/latest";

function mb(bytes: number) {
  return `${(bytes / 1048576).toFixed(1)} MB`;
}

/** Latest release from the GitHub API; falls back to plain links when offline or rate-limited. */
export default function Download({ c, lang }: { c: Copy; lang: "ko" | "en" }) {
  const [rel, setRel] = useState<Release | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const ctrl = new AbortController();
    fetch(API, { signal: ctrl.signal, headers: { Accept: "application/vnd.github+json" } })
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then((j: Release) => setRel(j))
      .catch(() => setFailed(true));
    return () => ctrl.abort();
  }, []);

  const sums = rel?.assets.find((a) => a.name === "SHA256SUMS");
  const date = rel ? new Date(rel.published_at).toLocaleDateString(lang === "ko" ? "ko-KR" : "en-GB") : "";

  return (
    <section id="download" className="section">
      <h2>{c.downloadTitle}</h2>
      <p className="intro">{c.downloadIntro}</p>
      <div className="release-head">
        <span className="release-label">{c.latest}</span>
        {rel ? (
          <a href={rel.html_url}>
            <strong>{rel.tag_name}</strong>
            {rel.prerelease ? " (pre-release)" : ""} · {date}
          </a>
        ) : (
          <a href={`${REPO}/releases/latest`}>{failed ? c.latestUnknown : "…"}</a>
        )}
      </div>
      <table className="assets">
        <tbody>
          {c.assets.map((kind) => {
            const a = rel?.assets.find((x) => kind.match.test(x.name));
            return (
              <tr key={kind.name}>
                <th scope="row">{kind.name}</th>
                <td className="note">{kind.note}</td>
                <td className="link">
                  {a ? (
                    <a href={a.browser_download_url} download>
                      {a.name} <span className="size">{mb(a.size)}</span>
                    </a>
                  ) : (
                    <a href={`${REPO}/releases/latest`}>GitHub Releases</a>
                  )}
                </td>
              </tr>
            );
          })}
          <tr>
            <th scope="row">{c.checksums}</th>
            <td className="note">SHA-256</td>
            <td className="link">
              <a href={sums ? sums.browser_download_url : `${REPO}/releases/latest`}>SHA256SUMS</a>
            </td>
          </tr>
        </tbody>
      </table>
      <p className="fine">{c.smartscreen}</p>
      <p className="fine">
        <a href={`${REPO}/blob/main/BUILD.md`}>{c.buildFromSource}</a>
        {" — "}
        <code>cargo build --release</code>
      </p>
    </section>
  );
}
