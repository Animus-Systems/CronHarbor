<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isExecution, isDiagnostic, isWorkload } from "../guards";
import type {
  DiagnosticDto,
  ExecutionDto,
  WorkloadDto,
} from "../generated/contracts";
import { time, isService } from "../format";
import StateBadge from "../components/StateBadge.vue";
import LogViewer from "../components/LogViewer.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
const id = String(useRoute().params.id);
const router = useRouter();
const execution = ref<ExecutionDto>();
const diagnostic = ref<DiagnosticDto>();
const fetchedWorkload = ref<WorkloadDto>();
const targetWorkload = computed(
  () =>
    d.workloads.value.find(
      (item) => item.id === execution.value?.workload_id,
    ) ?? fetchedWorkload.value,
);
const cancelCapability = computed(() => {
  const kind = targetWorkload.value?.kind.toLowerCase();
  if (!kind) return null;
  if (isService(kind)) return "service.control_preapproved";
  if (
    ["scheduled_job", "one_shot_job", "scheduledjob", "oneshotjob"].includes(
      kind,
    )
  )
    return "workload.run_preapproved";
  return null;
});
const loading = ref(true);
const error = ref("");
const diagnosticError = ref("");
const action = ref("");
const reconcileState = ref("FAILED");
let stopped = false;
let fetching = false;
let timer: ReturnType<typeof setTimeout> | undefined;
const unknown = computed(
  () => execution.value?.state.toLowerCase() === "unknown",
);
const retryPending = computed(() => diagnostic.value?.safe_to_retry_automatically === true);
const terminal = computed(() =>
  [
    "succeeded",
    "failed",
    "timed_out",
    "cancelled",
    "canceled",
    "skipped",
  ].includes(execution.value?.state.toLowerCase() ?? ""),
);
async function load(): Promise<void> {
  if (fetching || stopped) return;
  fetching = true;
  if (timer) clearTimeout(timer);
  try {
    const result = await d
      .client()
      .get(`/executions/${encodeURIComponent(id)}`, isExecution);
    if (stopped) return;
    execution.value = result;
    error.value = "";
    if (!targetWorkload.value && d.can("workload.read", result.workload_id)) {
      try {
        fetchedWorkload.value = await d
          .client()
          .get(
            `/workloads/${encodeURIComponent(result.workload_id)}`,
            isWorkload,
          );
      } catch {
        fetchedWorkload.value = undefined;
      }
    }
    try {
      const result = await d
        .client()
        .get(`/diagnostics/${encodeURIComponent(id)}`, isDiagnostic);
      if (!stopped) {
        diagnostic.value = result;
        diagnosticError.value = "";
      }
    } catch (caught) {
      diagnosticError.value = formatError(caught);
    }
  } catch (caught) {
    error.value = formatError(caught);
  } finally {
    fetching = false;
    loading.value = false;
    if (
      !stopped &&
      (!terminal.value || retryPending.value || Boolean(diagnosticError.value))
    )
      timer = setTimeout(() => {
        if (d.connected.value) void load();
      }, 2500);
  }
}
watch(
  () => d.lastRefresh.value,
  () => { void load(); },
);
watch(
  () => d.connected.value,
  (connected) => {
    if (connected && execution.value) void load();
  },
);
watch(
  () => d.executions.value.find((item) => item.execution_id === id),
  (latest) => {
    if (
      latest &&
      execution.value &&
      (latest.attempt_id !== execution.value.attempt_id ||
        latest.state !== execution.value.state)
    )
      void load();
  },
);
onMounted(() => {
  void load();
});
onBeforeUnmount(() => {
  stopped = true;
  if (timer) clearTimeout(timer);
});
async function confirm(reason: string): Promise<void> {
  const requested = action.value;
  action.value = "";
  const operation = await d.mutate(
    `/executions/${encodeURIComponent(id)}/${requested}`,
    requested === "reconcile"
      ? { state: reconcileState.value, evidence: reason }
      : {},
  );
  if (operation)
    void router.push(`/operations/${encodeURIComponent(operation.id)}`);
}
</script>
<template>
  <RouterLink to="/executions" class="back-link">← Executions</RouterLink>
  <p v-if="loading" class="loading" role="status">
    Loading execution evidence…
  </p>
  <p v-else-if="error" class="message danger" role="alert">{{ error }}</p>
  <template v-else-if="execution"
    ><section class="page-heading">
      <div>
        <p class="eyebrow">{{ execution.namespace }} / EXECUTION</p>
        <h1>{{ execution.name }}<span class="heading-dot">.</span></h1>
        <p class="muted mono wrap">{{ execution.execution_id }}</p>
      </div>
      <StateBadge :state="execution.state" />
    </section>
    <div v-if="retryPending" class="message warning">
      <strong>Retry queued.</strong> The last attempt failed. Cancelling removes the pending retry and preserves that attempt's evidence.
    </div>
    <div v-if="unknown" class="message warning">
      <strong>UNKNOWN is an uncertain outcome.</strong> The daemon cannot prove
      completion or containment. Do not repeat external side effects until
      reliable evidence establishes the previous outcome.
    </div>
    <div class="action-bar">
      <RouterLink
        class="button"
        :to="`/jobs/${encodeURIComponent(execution.workload_id)}`"
        >Approved workload</RouterLink
      ><button
        v-if="(!terminal || retryPending) && !unknown"
        class="danger-button"
        :disabled="
          !d.canAct.value ||
          !cancelCapability ||
          !d.can(cancelCapability, execution.workload_id)
        "
        @click="action = 'cancel'"
      >
        Cancel execution</button
      ><template v-if="unknown"
        ><label
          ><span class="sr-only">Reconciled state</span
          ><select v-model="reconcileState">
            <option value="FAILED">Verified failed</option>
            <option value="SUCCEEDED">Verified succeeded</option>
            <option value="CANCELLED">Verified cancelled</option>
            <option value="TIMED_OUT">Verified timed out</option>
          </select></label
        ><button
          class="danger-button"
          :disabled="!d.canAct.value || !d.can('operations.manage')"
          @click="action = 'reconcile'"
        >
          Reconcile with evidence
        </button></template
      >
    </div>
    <p
      v-if="!terminal && !unknown && !cancelCapability"
      class="permission-note"
    >
      The workload kind could not be verified. Cancellation remains disabled
      until its required capability is known.
    </p>
    <div class="two-column">
      <section class="panel">
        <div class="panel-heading"><h2>Attempt timeline</h2></div>
        <ol class="timeline">
          <li>
            <span class="timeline-marker" />
            <div>
              <strong>Scheduled due</strong
              ><time>{{ time(execution.due_at_us, true) }}</time>
            </div>
          </li>
          <li>
            <span class="timeline-marker" />
            <div>
              <strong>Started</strong
              ><time>{{ time(execution.started_at_us, true) }}</time>
            </div>
          </li>
          <li>
            <span class="timeline-marker" />
            <div>
              <strong>{{
                terminal
                  ? "Finished"
                  : unknown
                    ? "Final outcome unverified"
                    : "Awaiting terminal observation"
              }}</strong
              ><time>{{ time(execution.finished_at_us, true) }}</time>
            </div>
          </li>
        </ol>
        <dl class="detail-grid">
          <div>
            <dt>Attempt ID</dt>
            <dd class="mono wrap">{{ execution.attempt_id }}</dd>
          </div>
          <div>
            <dt>Exit code</dt>
            <dd>{{ execution.exit_code ?? "Not observed" }}</dd>
          </div>
          <div>
            <dt>Error code</dt>
            <dd>{{ execution.error_code ?? "None recorded" }}</dd>
          </div>
          <div>
            <dt>Output completeness</dt>
            <dd>
              {{
                execution.log_truncated
                  ? "Stored log truncated"
                  : "No stored truncation flag; crash output can still be incomplete"
              }}
            </dd>
          </div>
        </dl>
      </section>
      <section class="panel">
        <div class="panel-heading">
          <div>
            <h2>Deterministic diagnostics</h2>
            <p>Observations and hypotheses kept separate</p>
          </div>
        </div>
        <p v-if="diagnosticError" class="message warning">
          {{ diagnosticError }}
        </p>
        <div v-else-if="diagnostic" class="panel-content">
          <h3>Observed causes</h3>
          <ul v-if="diagnostic.observed_causes.length" class="plain-list">
            <li v-for="cause in diagnostic.observed_causes" :key="cause">
              {{ cause }}
            </li>
          </ul>
          <p v-else class="muted">No observed causes recorded.</p>
          <h3>Hypotheses · unverified</h3>
          <ul v-if="diagnostic.hypotheses.length" class="plain-list">
            <li v-for="hypothesis in diagnostic.hypotheses" :key="hypothesis">
              {{ hypothesis }}
            </li>
          </ul>
          <p v-else class="muted">No hypotheses supplied.</p>
          <p class="hint">
            Automatic retry assessment:
            {{
              diagnostic.safe_to_retry_automatically
                ? "server marked safe under current policy"
                : "not established safe"
            }}. This screen never automatically retries an execution.
          </p>
        </div>
      </section>
    </div>
    <LogViewer
      :key="execution.attempt_id"
      :execution-id="execution.execution_id"
      :attempt-id="execution.attempt_id"
      :workload-id="execution.workload_id"
      :terminal="terminal"
    />
    <section class="panel">
      <div class="panel-heading">
        <h2>Evidence records</h2>
        <span class="count">{{ diagnostic?.evidence.length ?? 0 }}</span>
      </div>
      <div v-if="!diagnostic?.evidence.length" class="inline-empty">
        No event evidence available for this execution.
      </div>
      <div v-else class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Evidence ID</th>
              <th>Observed event</th>
              <th>Time · UTC</th>
              <th>Data</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="event in diagnostic.evidence" :key="event.seq">
              <td class="mono">{{ event.seq }}</td>
              <td>{{ event.event_kind }}</td>
              <td>{{ time(event.created_at_us, true) }}</td>
              <td>
                <details>
                  <summary>Inspect data</summary>
                  <pre class="evidence-data">{{ event.payload_json }}</pre>
                </details>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
    <ConfirmDialog
      :open="Boolean(action)"
      :title="
        action === 'reconcile'
          ? 'Record a verified outcome?'
          : 'Cancel this execution?'
      "
      :impact="
        action === 'reconcile'
          ? 'Reconciliation changes the recorded outcome. Supply independent evidence that establishes process containment and external side effects. A guess does not become evidence because it fits in this box.'
          : retryPending
            ? 'Cancellation removes the pending retry. The last failed attempt remains in history.'
            : 'Cancellation requests termination. External side effects may already have occurred. Completion remains uncertain until the executor confirms containment.'
      "
      :reason-required="action === 'reconcile'"
      :confirm-label="
        action === 'reconcile'
          ? 'Record verified outcome'
          : 'Request cancellation'
      "
      @cancel="action = ''"
      @confirm="confirm"
    />
  </template>
</template>
