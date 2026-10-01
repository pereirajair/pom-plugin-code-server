import { useCallback, useEffect, useRef, useState } from "react";
import { usePluginI18n } from "../host/runtime";

const PROXY = "/api/ui/plugins/code_server/proxy";

type RuntimeStatus = { status?: string; error?: string; detail?: { version?: string } };

export function Editor() {
  const { t } = usePluginI18n();
  const [state, setState] = useState<"starting" | "ready" | "error">("starting");
  const [message, setMessage] = useState("");
  const [canRestart, setCanRestart] = useState(false);
  const [reload, setReload] = useState(0);
  const startedAt = useRef(Date.now());

  const poll = useCallback(async (signal: AbortSignal) => {
    try {
      const response = await fetch(`${PROXY}/_pom/status`, { cache: "no-store", signal });
      if (!response.ok) {
        if (Date.now() - startedAt.current > 300_000) {
          setState("error");
          setMessage(t("unavailableDetail"));
          setCanRestart(false);
        }
        return;
      }
      const body = (await response.json()) as RuntimeStatus;
      if (body.status === "ready") {
        setState("ready");
        setCanRestart(false);
        setMessage(body.detail?.version ? t("version", { version: body.detail.version }) : "");
      } else if (body.status === "error") {
        setState("error");
        setMessage(body.error ?? t("failedDetail"));
        setCanRestart(true);
      } else if (Date.now() - startedAt.current > 300_000) {
        setState("error");
        setMessage(t("unavailableDetail"));
        setCanRestart(true);
      } else {
        setState("starting");
      }
    } catch {
      // The node proxy reports 503 until host.configure finishes launching the IDE.
    }
  }, [t]);

  useEffect(() => {
    const controller = new AbortController();
    void poll(controller.signal);
    const timer = window.setInterval(() => void poll(controller.signal), 1500);
    return () => {
      controller.abort();
      window.clearInterval(timer);
    };
  }, [poll, reload]);

  const restart = async () => {
    setState("starting");
    setCanRestart(false);
    setMessage("");
    startedAt.current = Date.now();
    try {
      const response = await fetch(`${PROXY}/_pom/restart`, { method: "POST", cache: "no-store" });
      if (!response.ok) throw new Error("restart unavailable");
      setReload((value) => value + 1);
    } catch {
      setState("error");
      setCanRestart(false);
      setMessage(t("unavailableDetail"));
    }
  };

  return (
    <main className="cs-page">
      {state === "ready" ? (
        <iframe
          key={reload}
          className="cs-frame"
          src={`${PROXY}/`}
          title={t("frameTitle")}
          allow="clipboard-read; clipboard-write"
        />
      ) : (
        <section className="cs-status" role="status" aria-live="polite">
          {state === "starting" && <span className="cs-spinner" aria-hidden="true" />}
          <h1 className="cs-title">{state === "error" ? t("failed") : t("starting")}</h1>
          <p className="cs-detail">{state === "error" ? message : t("startingDetail")}</p>
          {state === "error" && canRestart && (
            <button className="cs-button" type="button" onClick={() => void restart()}>
              {t("retry")}
            </button>
          )}
        </section>
      )}
      {state === "ready" && message && <span className="cs-version">{message}</span>}
    </main>
  );
}
