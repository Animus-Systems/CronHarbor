<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isWorkload, isPreview, isExecution, pageOf, record } from "../guards";
import type {
  WorkloadDto,
  PreviewDto,
  ExecutionDto,
} from "../generated/contracts";
import { isService, shortId, time } from "../format";
import StateBadge from "../components/StateBadge.vue";
import ExecutionTable from "../components/ExecutionTable.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
const route = useRoute();
const router = useRouter();
const id = String(route.params.id);
const workload = ref<WorkloadDto>();
const preview = ref<PreviewDto>();
const executions = ref<ExecutionDto[]>([]);
const error = ref("");
const executionError = ref("");
const loading = ref(true);
const previewError = ref("");
const action = ref("");
const dispatchBlocked = computed(() =>
  Boolean(d.status.value?.paused || d.status.value?.restore_required),
);
const service = computed(() =>
  workload.value ? isService(workload.value.kind) : false,
);
const fileOwned = computed(
  () => workload.value?.source_kind.toLowerCase() !== "api",
);
const servicePolicy = computed(() => {
  try {
    const value: unknown = JSON.parse(workload.value?.definition ?? "");
    return record(value) && record(value.spec) && record(value.spec.service)
      ? value.spec.service
      : null;
  } catch {
    return null;
  }
});
const runCapability = computed(() =>
  service.value ? "service.control_preapproved" : "workload.run_preapproved",
);
const actionTitle = computed(
  () =>
    ({
      run: "Run this job now?",
      pause: workload.value?.paused
        ? "Resume scheduling?"
        : "Pause scheduling?",
      start: "Start this service?",
      stop: "Stop this service?",
      restart: "Restart this service?",
    })[action.value] ?? "Confirm operation",
);
const impact = computed(() =>
  action.value === "run"
    ? "This starts a new execution of the approved definition. External side effects may be repeated. Confirm that running it now is safe."
    : ["stop", "restart"].includes(action.value)
      ? "This interrupts the managed service and may affect active requests. The operation is asynchronous; acceptance does not establish readiness."
      : action.value === "pause"
        ? "This changes future dispatch for this workload. Existing executions are not cancelled."
        : "This changes desired state. Observed readiness must be verified separately.",
);
let stopped = false;
let detailRefreshing = false;
async function load(): Promise<void> {
  const current = d.api.value;
  if (stopped || detailRefreshing || !d.connected.value || !current) return;
  const path = route.path;
  const ownsRequest = () =>
    !stopped && d.api.value === current && route.path === path;
  detailRefreshing = true;
  try {
    const result = await current.get(
      `/workloads/${encodeURIComponent(id)}`,
      isWorkload,
    );
    if (!ownsRequest()) return;
    workload.value = result;
    error.value = "";
    if (d.can("execution.read")) {
      try {
        const result = await current.get(
          `/executions?workload_id=${encodeURIComponent(id)}&limit=20`,
          pageOf(isExecution),
        );
        if (!ownsRequest()) return;
        executions.value = result.items;
        executionError.value = "";
      } catch (caught) {
        if (!ownsRequest()) return;
        executionError.value = formatError(caught);
      }
    } else {
      executions.value = [];
      executionError.value = "Execution evidence requires execution.read.";
    }
    if (!service.value && workload.value.schedule_description) {
      try {
        const result = await current.get(
          `/schedules/${encodeURIComponent(id)}/preview?count=10`,
          isPreview,
        );
        if (!ownsRequest()) return;
        preview.value = result;
        previewError.value = "";
      } catch (caught) {
        if (!ownsRequest()) return;
        previewError.value = formatError(caught);
      }
    } else {
      preview.value = undefined;
      previewError.value = "";
    }
  } catch (caught) {
    if (ownsRequest()) error.value = formatError(caught);
  } finally {
    if (ownsRequest()) loading.value = false;
    detailRefreshing = false;
  }
}
onMounted(() => {
  void load();
});
onBeforeUnmount(() => {
  stopped = true;
});
watch(
  () => d.lastRefresh.value,
  () => {
    void load();
  },
);
async function confirm(): Promise<void> {
  const requested = action.value;
  action.value = "";
  let path: string;
  let body: unknown = {};
  if (requested === "run") path = `/jobs/${encodeURIComponent(id)}/run`;
  else if (requested === "pause") {
    path = `/workloads/${encodeURIComponent(id)}/pause`;
    body = { paused: !workload.value?.paused };
  } else if (requested === "restart") {
    path = `/services/${encodeURIComponent(id)}/restart`;
    body = { expected_revision: workload.value?.revision_id };
  } else {
    path = `/services/${encodeURIComponent(id)}/desired-state`;
    body = {
      desired_state: requested === "start" ? "RUNNING" : "STOPPED",
      expected_revision: workload.value?.revision_id,
    };
  }
  const operation = await d.mutate(path, body);
  if (operation)
    void router.push(`/operations/${encodeURIComponent(operation.id)}`);
}
</script>
<template>
  <p v-if="loading" class="loading" role="status">
    Loading workload and authoritative revision…
  </p>
  <p v-else-if="error" class="message danger" role="alert">{{ error }}</p>
  <template v-else-if="workload"
    ><RouterLink :to="service ? '/services' : '/jobs'" class="back-link"
      >← {{ service ? "Services" : "Jobs" }}</RouterLink
    >
    <section class="page-heading">
      <div>
        <p class="eyebrow">{{ workload.namespace }} / {{ workload.kind }}</p>
        <h1>{{ workload.name }}<span class="heading-dot">.</span></h1>
        <p class="muted mono">{{ workload.id }}</p>
      </div>
      <StateBadge
        :state="
          workload.paused
            ? 'paused'
            : service
              ? (workload.observed_state ?? 'unknown')
              : workload.enabled
                ? 'enabled'
                : 'disabled'
        "
      />
    </section>
    <div v-if="dispatchBlocked" class="message warning">
      Global dispatch is paused or restore quarantine is active. New starts and
      restarts are disabled; verify and resume through the trusted
      administrative CLI.
    </div>
    <div class="action-bar">
      <template v-if="service"
        ><button
          class="primary"
          :disabled="
            !d.canAct.value || dispatchBlocked || !d.can(runCapability, id)
          "
          @click="action = 'start'"
        >
          Start</button
        ><button
          :disabled="!d.canAct.value || !d.can(runCapability, id)"
          @click="action = 'stop'"
        >
          Stop</button
        ><button
          :disabled="
            !d.canAct.value || dispatchBlocked || !d.can(runCapability, id)
          "
          @click="action = 'restart'"
        >
          Restart service
        </button></template
      ><button
        v-else
        class="primary"
        :disabled="
          !d.canAct.value || dispatchBlocked || !d.can(runCapability, id)
        "
        @click="action = 'run'"
      >
        Run now</button
      ><button
        v-if="!service"
        :disabled="!d.canAct.value || !d.can('workload.run_preapproved', id)"
        @click="action = 'pause'"
      >
        {{ workload.paused ? "Resume schedule" : "Pause schedule" }}</button
      ><RouterLink
        v-if="!fileOwned && d.can('workload.edit_safe_config', id)"
        :to="`/jobs/${encodeURIComponent(id)}/edit`"
        class="button"
        >Edit definition</RouterLink
      ><button v-else disabled>
        {{
          fileOwned
            ? workload.source_kind.toLowerCase() === "file"
              ? "File-owned · read only"
              : "Source-owned · read only"
            : "Edit permission required"
        }}
      </button>
    </div>
    <p v-if="!d.can(runCapability, id)" class="permission-note">
      Requires {{ runCapability }}. The daemon enforces all permissions.
    </p>
    <div class="two-column">
      <section class="panel">
        <div class="panel-heading">
          <h2>Workload contract</h2>
          <span class="source-badge">{{
            workload.source_kind.toUpperCase()
          }}</span>
        </div>
        <dl class="detail-grid">
          <div>
            <dt>Revision</dt>
            <dd class="mono" :title="workload.revision_id">
              {{ shortId(workload.revision_id) }}
            </dd>
          </div>
          <div>
            <dt>Executor</dt>
            <dd>{{ workload.executor }}</dd>
          </div>
          <div>
            <dt>Schedule</dt>
            <dd>
              {{ workload.schedule_description ?? "No recurring schedule" }}
            </dd>
          </div>
          <div>
            <dt>Next due · local</dt>
            <dd>{{ time(workload.next_due_at_us) }}</dd>
          </div>
          <template v-if="service"
            ><div>
              <dt>Desired state</dt>
              <dd>{{ workload.desired_state }}</dd>
            </div>
            <div>
              <dt>Observed state</dt>
              <dd>
                <StateBadge :state="workload.observed_state ?? 'unknown'" />
              </dd>
            </div>
            <div>
              <dt>Restarts observed</dt>
              <dd>{{ workload.restart_count }}</dd>
            </div>
            <div>
              <dt>Restart budget</dt>
              <dd>
                {{ servicePolicy?.maxRestarts ?? "Unavailable" }} restarts /
                {{ servicePolicy?.windowSeconds ?? "—" }} seconds
              </dd>
            </div>
            <div>
              <dt>Readiness probe</dt>
              <dd>
                {{
                  servicePolicy?.readiness
                    ? "Configured · inspect execution evidence"
                    : "No readiness probe configured"
                }}
              </dd>
            </div>
            <div>
              <dt>Restart policy</dt>
              <dd>
                {{ servicePolicy?.restart ?? "Unavailable" }} · backoff
                {{ servicePolicy?.backoffSeconds ?? "—" }}–{{
                  servicePolicy?.maxBackoffSeconds ?? "—"
                }}
                seconds
              </dd>
            </div></template
          >
        </dl>
      </section>
      <section v-if="!service" class="panel">
        <div class="panel-heading">
          <div>
            <h2>Schedule preview</h2>
            <p>
              {{ preview?.timezone ?? "UTC" }} policy · UTC and browser local
              time
            </p>
          </div>
        </div>
        <p v-if="previewError" class="message warning">{{ previewError }}</p>
        <div v-else-if="!preview?.items.length" class="inline-empty">
          No recurring occurrences to preview.
        </div>
        <ol v-else class="preview-list">
          <li v-for="instant in preview.items" :key="instant.utc_us">
            <span class="mono">{{ time(instant.utc_us, true) }}</span
            ><small>{{ time(instant.utc_us) }} · {{ instant.local }}</small>
          </li>
        </ol>
      </section>
      <section v-else class="panel">
        <div class="panel-heading"><h2>Lifecycle boundaries</h2></div>
        <div class="panel-content">
          <p>
            Desired state records intent. Observed state and readiness record
            what the executor can verify.
          </p>
          <p class="muted">
            Restart limits and readiness probes belong to the approved
            definition. A restart request returns an operation; follow it until
            a final outcome.
          </p>
          <p class="muted">
            External Docker or systemd ownership requires explicit adapter
            permission and configured allowlists.
          </p>
        </div>
      </section>
    </div>
    <section v-if="service" class="panel">
      <div class="panel-heading">
        <div>
          <h2>Readiness and liveness evidence</h2>
          <p>
            Recent observed probe results · absence is unverified, not healthy
          </p>
        </div>
      </div>
      <div v-if="!workload.probe_history.length" class="inline-empty">
        No probe results observed for this service.
      </div>
      <div v-else class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Probe</th>
              <th>Observed result</th>
              <th>Observation · UTC</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="(probe, index) in workload.probe_history"
              :key="`${probe.observed_at_us}-${index}`"
            >
              <td>{{ probe.kind }}</td>
              <td>
                <StateBadge :state="probe.healthy ? 'passed' : 'failed'" />
              </td>
              <td>{{ time(probe.observed_at_us, true) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>Recent executions</h2>
          <p>Immutable attempt records for this workload</p>
        </div>
      </div>
      <p v-if="executionError" class="message warning">{{ executionError }}</p>
      <ExecutionTable v-else-if="executions.length" :items="executions" />
      <div v-else class="inline-empty">
        No executions recorded yet. A successful apply creates a revision; it
        does not run the job.
      </div>
    </section>
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>Approved definition</h2>
          <p>
            {{
              fileOwned
                ? "Source-owned: update the source file, review config plan, then apply through the trusted CLI."
                : "API-owned: changes require a reviewed plan and current-revision check."
            }}
          </p>
        </div>
      </div>
      <pre class="code-block">{{ workload.definition }}</pre>
    </section>
    <ConfirmDialog
      :open="Boolean(action)"
      :title="actionTitle"
      :impact="impact"
      @cancel="action = ''"
      @confirm="confirm"
    />
  </template>
</template>
