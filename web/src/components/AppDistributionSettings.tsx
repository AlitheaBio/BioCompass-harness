import { useEffect, useState } from "react";

type Release = {
  state: "unconfigured" | "choose_target" | "unavailable" | "current" | "update" | "download";
  current_version: string | null;
  target: string | null;
  version?: string;
  notes?: string;
  filename?: string;
  pin?: string;
  node?: {version: string; filename: string; notes: string};
};
const systems: Record<string, string> = {
  "darwin-aarch64": "macOS · Apple silicon", "darwin-x86_64": "macOS · Intel",
  "linux-x86_64": "Linux · x86-64", "linux-aarch64": "Linux · ARM64",
  "windows-x86_64": "Windows · x64", "windows-aarch64": "Windows · ARM64",
};

async function browserTarget() {
  type Hints = { getHighEntropyValues(keys: string[]): Promise<{ platform?: string; architecture?: string; bitness?: string }> };
  const hints = (navigator as Navigator & { userAgentData?: Hints }).userAgentData;
  const data = await hints?.getHighEntropyValues(["platform", "architecture", "bitness"]);
  if (!data || data.bitness !== "64") return "";
  const os = ({ macOS: "darwin", Linux: "linux", Windows: "windows" } as Record<string,string>)[data.platform ?? ""];
  const cpu = ({ arm: "aarch64", x86: "x86_64" } as Record<string,string>)[data.architecture ?? ""];
  return os && cpu ? `${os}-${cpu}` : "";
}
function installation(filename: string) {
  if (/\.(zip|tar\.gz|tgz)$/i.test(filename)) return "Extract the archive and start BioCompass from the extracted directory.";
  if (/\.(dmg|exe|msi|pkg|deb|rpm)$/i.test(filename)) return "Open the verified package with your operating system’s installer.";
  return "Save the verified release and follow its installation notes.";
}

export function AppDistributionSettings({ local }: {local: boolean}) {
  const [target, setTarget] = useState<string>();
  const [release, setRelease] = useState<Release>();
  const [revision, setRevision] = useState(0);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState<"app"|"froglet-node"|null>(null);
  const [download, setDownload] = useState<AbortController | null>(null);
  useEffect(() => {
    let active = true;
    if (local) setTarget("");
    else void browserTarget().catch(() => "").then(value => { if (active) setTarget(value); });
    return () => { active = false; };
  }, [local]);
  useEffect(() => {
    if (target === undefined) return;
    const controller = new AbortController();
    setError(""); setRelease(undefined); setSaved(null);
    void fetch(`/api/v1/app-distribution?target=${encodeURIComponent(target)}`, { signal: controller.signal, cache: "no-store" })
      .then(async response => {
        if (!response.ok) {
          throw Error(response.status === 409 ? "The publisher signature on the release information could not be verified." : "Release information is unavailable. You can keep using this workspace.");
        }
        const value = await response.json() as Release;
        if (!controller.signal.aborted) setRelease(value);
      }).catch((reason: Error) => { if (!controller.signal.aborted) setError(reason.message); });
    return () => controller.abort();
  }, [target, revision]);
  useEffect(() => () => download?.abort(), [download]);

  async function save(product: "app"|"froglet-node" = "app") {
    const filename=product === "froglet-node" ? release?.node?.filename : release?.filename;
    if (!release?.target || !release.pin || !filename) return;
    const controller = new AbortController();
    setDownload(controller); setError(""); setSaved(null);
    try {
      const params = new URLSearchParams({ target: release.target, pin: release.pin, product });
      const response = await fetch(`/api/v1/app-distribution/download?${params}`, { signal: controller.signal, cache: "no-store" });
      if (!response.ok) {
        const failure: { code?: string } | null = await response.json().catch(() => null);
        if (failure?.code === "INTEGRITY_VIOLATION") throw Error("The publisher signature on the release file could not be verified.");
        if (response.status === 409) throw Error("A new release was published. Check again before downloading.");
        throw Error("The release could not be downloaded and verified. Retry when the channel is available.");
      }
      const file = await response.blob();
      if (controller.signal.aborted) return;
      const address = URL.createObjectURL(file);
      const link = document.createElement("a"); link.href = address; link.download = filename;
      document.body.append(link); link.click(); link.remove();
      window.setTimeout(() => URL.revokeObjectURL(address), 30000);
      setSaved(product);
    } catch (reason) {
      setError(controller.signal.aborted ? "Download canceled. You can retry." : reason instanceof Error ? reason.message : "Download interrupted. You can retry.");
    } finally { setDownload(null); }
  }
  return <section className="bc-card bc-settings-stack" aria-label="Local app">
    <strong>Local app</strong>
    {!local && target !== undefined && release?.state !== "unconfigured" && <label className="bc-field">Operating system and processor
      <select className="bc-select" value={target} disabled={!!download} onChange={e => setTarget(e.target.value)}>
        <option value="">Choose your platform</option>
        {Object.entries(systems).map(([id,name]) => <option value={id} key={id}>{name}</option>)}
      </select>
    </label>}
    {release?.current_version && <span>Installed version {release.current_version}</span>}
    {release?.state === "unconfigured" && <span>Your administrator has not configured a release channel.</span>}
    {release?.state === "unavailable" && <span>No release is available for {systems[release.target ?? ""] ?? "this platform"}.</span>}
    {release?.state === "current" && <span>Your local app is up to date.</span>}
    {release && (release.state === "download" || release.state === "update") && <>
      <span>Available version {release.version} · {systems[release.target ?? ""]}</span>
      {release.notes && <p style={{ whiteSpace: "pre-wrap" }}>{release.notes}</p>}
      <div><button className="bc-btn bc-btn-primary" type="button" disabled={!!download} onClick={() => void save()}>
        {download ? "Verifying download…" : local ? "Download verified update" : "Download local app"}
      </button>{download && <button className="bc-btn" type="button" onClick={() => download.abort()}>Cancel download</button>}</div>
      {saved === "app" && <p role="status">Verified download started. {installation(release.filename ?? "")}</p>}
    </>}
    {release?.node && <section aria-label="Customer node" className="bc-settings-stack">
      <strong>Customer node</strong>
      <span>BioCompass Froglet worker {release.node.version} · {systems[release.target ?? ""]}</span>
      {release.node.notes && <p>{release.node.notes}</p>}
      <button type="button" className="bc-btn" disabled={!!download} onClick={()=>void save("froglet-node")}>Download customer node</button>
      {saved === "froglet-node" && <p role="status">Verified customer node download started.</p>}
      <details><summary>Connect a customer node</summary>
        <p>Extract the worker on the customer machine. Keep a private state directory and generate its public key with <code>froglet-node pubkey --state-dir &lt;directory&gt;</code>.</p>
        <p>To add a compute worker, ask the operator of the hub to enroll its public key as a cloud-pool worker in the Platform Control Tower. Keep the returned credential on the node.</p>
      </details>
    </section>}
    {!release && !error && <p role="status">Checking release channel…</p>}
    {error && <p role="alert">{error}</p>}
    {(error || (local && release?.state !== "unconfigured")) && <button className="bc-btn" type="button" disabled={!!download} onClick={() => setRevision(v => v + 1)}>Check for updates</button>}
  </section>;
}
