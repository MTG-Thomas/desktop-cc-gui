import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/base/buttons/button";
import { Input } from "@/components/base/input/input";
import {
  SettingsCard,
  SettingsRow,
  SettingsSectionLabel,
} from "@/components/application/settings/settings-rows";
import { ipc, type AppSettings, type SshHost } from "@/lib/ipc";
import { enginesSummary, newSshHost, parsePort } from "./sshHosts";

/**
 * Enrolled plain-Linux SSH hosts (spike). Hosts are stored in settings;
 * attaching a workspace stamps its row so sends run over the SSH transport.
 * Auth is key-based only — the backend never prompts for a password.
 */
export function SshHostsSection() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [hosts, setHosts] = useState<SshHost[]>([]);
  const [hostDraft, setHostDraft] = useState("");
  const [userDraft, setUserDraft] = useState("root");
  const [portDraft, setPortDraft] = useState("22");
  const [wsDraft, setWsDraft] = useState("");
  const [remoteDraft, setRemoteDraft] = useState("");
  const [attachHostId, setAttachHostId] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getAppSettings()
      .then((s) => {
        if (cancelled) return;
        setSettings(s);
        setHosts(s.sshHosts ?? []);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 4000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const persist = useCallback(
    async (next: SshHost[]) => {
      if (!settings) return;
      setBusy(true);
      setError(null);
      try {
        const fresh = await ipc.getAppSettings();
        await ipc.updateAppSettings({ ...fresh, sshHosts: next });
        setSettings({ ...fresh, sshHosts: next });
        setHosts(next);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [settings],
  );

  const probeAndSave = useCallback(async () => {
    const port = parsePort(portDraft);
    if (!hostDraft.trim() || !userDraft.trim() || port === null) {
      setError(t("settings.sshHosts.invalid"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const probe = await ipc.sshHostProbe(hostDraft.trim(), userDraft.trim(), port);
      if (!probe.reachable) throw new Error(probe.error ?? t("settings.sshHosts.unreachable"));
      const host: SshHost = {
        ...newSshHost(hostDraft, userDraft, port),
        engines: probe.engines,
        lastOk: true,
        lastProbe: new Date().toISOString(),
      };
      await persist([...hosts, host]);
      setHostDraft("");
      setNotice(t("settings.sshHosts.added", { host: host.host }));
      if (!attachHostId) setAttachHostId(host.id);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [hostDraft, userDraft, portDraft, hosts, attachHostId, persist, t]);

  const removeHost = useCallback(
    (id: string) => {
      void persist(hosts.filter((h) => h.id !== id));
      if (attachHostId === id) setAttachHostId("");
    },
    [hosts, attachHostId, persist],
  );

  const attach = useCallback(async () => {
    const host = hosts.find((h) => h.id === attachHostId);
    if (!host || !wsDraft.trim() || !remoteDraft.trim()) {
      setError(t("settings.sshHosts.invalidAttach"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await ipc.sshHostAttach({
        host: host.host,
        user: host.user,
        port: host.port,
        workspacePath: wsDraft.trim(),
        remotePath: remoteDraft.trim(),
      });
      const found = Object.keys(res.engines).sort().join(", ") || t("settings.sshHosts.none");
      setNotice(t("settings.sshHosts.attached", { path: wsDraft.trim(), engines: found }));
      setWsDraft("");
      setRemoteDraft("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [hosts, attachHostId, wsDraft, remoteDraft, t]);

  return (
    <SettingsCard>
      <SettingsSectionLabel>{t("settings.sshHosts.title")}</SettingsSectionLabel>
      <p>{t("settings.sshHosts.desc")}</p>
      {hosts.map((h) => (
        <SettingsRow key={h.id} label={`${h.user}@${h.host}:${h.port}`}>
          <span>
            {h.lastOk ? enginesSummary(h.engines) || t("settings.sshHosts.none") : t("settings.sshHosts.unprobed")}
          </span>
          <Button disabled={busy} onClick={() => removeHost(h.id)}>
            {t("settings.sshHosts.remove")}
          </Button>
        </SettingsRow>
      ))}
      <SettingsRow label={t("settings.sshHosts.host")}>
        <Input value={hostDraft} onChange={(v) => setHostDraft(v)} placeholder="172.16.15.168" />
      </SettingsRow>
      <SettingsRow label={t("settings.sshHosts.user")}>
        <Input value={userDraft} onChange={(v) => setUserDraft(v)} placeholder="root" />
      </SettingsRow>
      <SettingsRow label={t("settings.sshHosts.port")}>
        <Input value={portDraft} onChange={(v) => setPortDraft(v)} placeholder="22" />
      </SettingsRow>
      <Button disabled={busy} onClick={() => void probeAndSave()}>
        {t("settings.sshHosts.probeAdd")}
      </Button>
      <SettingsSectionLabel>{t("settings.sshHosts.attachTitle")}</SettingsSectionLabel>
      <SettingsRow label={t("settings.sshHosts.attachHost")}>
        <select value={attachHostId} onChange={(e) => setAttachHostId(e.target.value)}>
          <option value="">{t("settings.sshHosts.chooseHost")}</option>
          {hosts.map((h) => (
            <option key={h.id} value={h.id}>
              {h.user}@{h.host}:{h.port}
            </option>
          ))}
        </select>
      </SettingsRow>
      <SettingsRow label={t("settings.sshHosts.localPath")}>
        <Input value={wsDraft} onChange={(v) => setWsDraft(v)} placeholder="C:\work\proj" />
      </SettingsRow>
      <SettingsRow label={t("settings.sshHosts.remotePath")}>
        <Input value={remoteDraft} onChange={(v) => setRemoteDraft(v)} placeholder="/home/user/proj" />
      </SettingsRow>
      <Button disabled={busy || !attachHostId} onClick={() => void attach()}>
        {t("settings.sshHosts.attach")}
      </Button>
      {error && <p role="alert">{error}</p>}
      {notice && <p>{notice}</p>}
    </SettingsCard>
  );
}
