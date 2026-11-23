import { useEffect, useState } from "react";
import { fetchPersonalProfile, formatErrorInline, requestPersonalEmail, savePersonalProfile, type PersonalProfile } from "../api";
import { isSafeHttpUrl } from "../safeUrl";

export function PersonalAvatar({ name, seed, size = 40 }: { name: string; seed?: string | null; size?: number }) {
  const color = seed ? parseInt(seed.slice(0, 4), 16) % 360 : 0;
  return (
    <span aria-label="Profile avatar" role="img" style={{
      display: "inline-grid", flex: "none", width: size, height: size, borderRadius: 4, overflow: "hidden",
      background: "var(--bc-bg-subtle)", placeItems: "center",
      gridTemplateColumns: seed ? "repeat(4, 1fr)" : undefined,
    }}>
      {seed ? Array.from({ length: 16 }, (_, i) => {
        const filled = parseInt(seed.charAt((i % 8) + 4), 16) & (1 << (i % 4));
        return <span aria-hidden="true" key={i} style={{
          width: "100%", height: "100%", backgroundColor: `hsl(${color} 63% ${filled ? 38 : 84}%)`,
        }} />;
      }) : <span aria-hidden="true">{name.trim()[0]?.toUpperCase() || "•"}</span>}
    </span>
  );
}

export function PersonalAccountSettings({ onProfileUpdated }: { onProfileUpdated?: (profile: PersonalProfile) => void }) {
  const [profile, setProfile] = useState<PersonalProfile>();
  const [name, setName] = useState("");
  const [avatar, setAvatar] = useState<string | null>(null);
  const [email, setEmail] = useState("");
  const [verification, setVerification] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    fetchPersonalProfile().then(p => {
      if (active) {setProfile(p); setName(p.display_name); setAvatar(p.avatar_seed); onProfileUpdated?.(p);}
    }).catch(e => {if (active) setMessage(formatErrorInline(e));});
    return () => {active = false;};
  }, [onProfileUpdated]);
  useEffect(() => {
    if (!verification || !profile?.requested_email) return;
    let active=true;
    function onFocus() {
      fetchPersonalProfile().then(p=> {
        if (!active) return;
        setProfile(p); onProfileUpdated?.(p);
        if (!p.requested_email) {setVerification(""); setEmail(""); setMessage("Email updated.");}
      }).catch(e=>{if(active)setMessage(formatErrorInline(e));});
    }
    window.addEventListener("focus", onFocus);
    return () => {active=false; window.removeEventListener("focus", onFocus);};
  }, [verification, profile?.requested_email, onProfileUpdated]);

  async function save() {
    setBusy(true);setMessage("");
    try {
      const saved=await savePersonalProfile(name,avatar);
      setProfile(saved); setName(saved.display_name); onProfileUpdated?.(saved); setMessage("Profile saved.");
    } catch(e) {setMessage(formatErrorInline(e));} finally {setBusy(false);}
  }
  async function changeEmail() {
    setBusy(true);setMessage("");setVerification("");
    try {
      const response=await requestPersonalEmail(email);
      setProfile(await fetchPersonalProfile());
      const safeVerify = isSafeHttpUrl(response.authorize_url, { allowRelative: false });
      if (!safeVerify) {
        setMessage("Identity provider returned an unsafe verification URL.");
      } else {
        setVerification(safeVerify);
      }
    } catch(e) {setMessage(formatErrorInline(e));} finally {setBusy(false);}
  }

  if (!profile) return <p role="status">{message || "Loading account…"}</p>;
  return <div className="bc-settings-stack">
    <div className="bc-card bc-settings-stack">
      <label className="bc-field">Display name
        <input className="bc-input" value={name} maxLength={120} onChange={e=>setName(e.target.value)}/>
      </label>
      <PersonalAvatar name={name} seed={avatar} />
      <div>
        <button className="bc-btn" type="button" onClick={()=>setAvatar(Array.from(crypto.getRandomValues(new Uint8Array(6)),v=>v.toString(16).padStart(2,"0")).join(""))}>Generate avatar</button>
        {avatar && <button className="bc-btn" type="button" onClick={()=>setAvatar(null)}>Remove avatar</button>}
      </div>
      <button className="bc-btn bc-btn-primary" type="button" disabled={busy || !name.trim() || (name===profile.display_name && avatar===profile.avatar_seed)} onClick={()=>void save()}>Save profile</button>
    </div>
    <div className="bc-card bc-settings-stack">
      <strong>Sign-in email</strong><span>{profile.email}</span>
      {profile.email_verification === "oidc" ? <>
        <label className="bc-field">New email address
          <input className="bc-input" type="email" value={email} onChange={e=>{setEmail(e.target.value);setVerification("");}}/>
        </label>
        <button className="bc-btn" type="button" disabled={busy || !email.trim()} onClick={()=>void changeEmail()}>Request verification</button>
        {isSafeHttpUrl(verification, { allowRelative: false }) && <a className="bc-btn" href={isSafeHttpUrl(verification, { allowRelative: false })!} target="_blank" rel="noopener noreferrer">Verify with your identity provider</a>}
        {profile.requested_email && <p>Waiting for your identity provider to verify {profile.requested_email}.</p>}
      </> : <p>Your account needs a linked email-verifying identity provider to change its sign-in email.</p>}
    </div>
    {message && <p role="status">{message}</p>}
  </div>;
}
