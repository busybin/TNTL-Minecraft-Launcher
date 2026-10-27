import { ref } from "vue";
import { events, formatBytes, jobTag, type InstallEvent } from "./api";
import { t } from "./i18n";
import { useTransfer } from "./progress";

/**
 * Install state lives outside the install page: an install is a durable Rust
 * task, so leaving the page must not drop its progress or allow a second one.
 */
export const installState = {
  installing: ref(false),
  stage: ref<InstallEvent | null>(null),
  progress: ref(0),
  result: ref<string | null>(null),
  installError: ref<string | null>(null),
  /** Instance that the finished install registered, used to open its detail. */
  doneInstance: ref<string | null>(null),
};

const transfer = useTransfer();
export const installSpeed = transfer.speed;
export const resetInstallTransfer = transfer.reset;

let listening = false;

/** Registers the global listeners once; call from the app shell. */
export async function startInstallListeners() {
  if (listening) return;
  listening = true;
  for (const listen of [
    () =>
      events.onInstallProgress(({ job, event }) => {
        if (job !== jobTag.install) return;
        installState.stage.value = event;
        const total = event.bytes_total && event.bytes_total > 0 ? event.bytes_total : event.total;
        const done = event.bytes_total && event.bytes_total > 0 ? event.bytes : event.done;
        transfer.track(done, total || null);
        if (total > 0) installState.progress.value = Math.min(100, (done / total) * 100);
      }),
    () =>
      events.onInstallDone((done) => {
        installState.installing.value = false;
        installState.progress.value = 100;
        installState.doneInstance.value = done.instance;
        installState.result.value = t("install.done", {
          version: done.version,
          files: done.downloaded,
          bytes: formatBytes(done.bytes),
          skipped: done.skipped,
        });
      }),
    () =>
      events.onInstallFailed((error) => {
        installState.installing.value = false;
        installState.installError.value = error;
        // Otherwise the bar keeps showing the last stage and a frozen speed
        // next to the error.
        installState.progress.value = 0;
        installState.stage.value = null;
        transfer.reset();
      }),
  ]) {
    try {
      await listen();
    } catch (cause) {
      installState.installError.value = String(cause);
    }
  }
}
