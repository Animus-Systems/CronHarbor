<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isPlan, isValidation, isWorkload } from "../guards";
import type { PlanDto, WorkloadDto } from "../generated/contracts";
import ConfirmDialog from "../components/ConfirmDialog.vue";
const route = useRoute();
const router = useRouter();
const kind = ref(String(route.query.kind ?? "ScheduledJob"));
const definition = ref("");
const original = ref<WorkloadDto>();
const plan = ref<PlanDto>();
const error = ref("");
const validated = ref(false);
const busy = ref(false);
const confirmOpen = ref(false);
const loading = ref(Boolean(route.params.id));
const sourceOwned = computed(() =>
  Boolean(original.value && original.value.source_kind.toLowerCase() !== "api"),
);
const editable = computed(
  () =>
    (!route.params.id || Boolean(original.value)) &&
    !sourceOwned.value &&
    d.can("workload.edit_safe_config", original.value?.id),
);
function template(value: string): string {
  const command = value === "Service" ? "/bin/sleep" : "/bin/echo";
  const args = value === "Service" ? '["3600"]' : '["Hello from CronHarbor"]';
  const service =
    value === "Service"
      ? "  service:\n    desiredState: STOPPED\n    restart: on-failure\n    maxRestarts: 5\n    windowSeconds: 300\n    stopGraceSeconds: 5\n"
      : "";
  return `apiVersion: cronharbor.dev/v1\nkind: ${value}\nmetadata:\n  name: hello-world\n  namespace: default\nspec:\n  owner: api\n  enabled: true\n  schedule:\n    type: cron\n    expression: "0 0 * * *"\n    timezone: UTC\n    fold: first\n    gap: skip\n    misfire:\n      policy: skip\n      latenessGraceSeconds: 60\n      maxAgeSeconds: 3600\n      maxOccurrences: 1\n  execution:\n    adapter: process\n    command: ${command}\n    args: ${args}\n    workingDirectory: /tmp\n    sandbox: default\n  concurrency:\n    mode: forbid\n    maxActive: 1\n  timeoutSeconds: 30\n  retry:\n    maxAttempts: 1\n  logs:\n    maxRunBytes: 65536\n${service}`;
}
onMounted(async () => {
  if (route.params.id) {
    try {
      original.value = await d
        .client()
        .get(
          `/workloads/${encodeURIComponent(String(route.params.id))}`,
          isWorkload,
        );
      definition.value = original.value.definition;
    } catch (caught) {
      error.value = formatError(caught);
    } finally {
      loading.value = false;
    }
  } else definition.value = template(kind.value);
});
watch(definition, () => {
  validated.value = false;
  plan.value = undefined;
});
function changeKind(): void {
  if (!original.value) definition.value = template(kind.value);
}
async function review(): Promise<void> {
  const reviewedDefinition = definition.value;
  const current = d.api.value;
  if (new TextEncoder().encode(definition.value).length > 65536) {
    error.value = "Definitions must be at most 64 KiB.";
    return;
  }
  busy.value = true;
  error.value = "";
  plan.value = undefined;
  try {
    const validation = await d
      .client()
      .post(
        "/workloads/validate",
        { definition: reviewedDefinition },
        isValidation,
      );
    if (d.api.value !== current || definition.value !== reviewedDefinition)
      return;
    if (!validation.valid) {
      error.value =
        "Definition validation failed. Check the supported schema and policy.";
      return;
    }
    validated.value = true;
    const result = await d
      .client()
      .post("/config/plan", { definition: reviewedDefinition }, isPlan);
    if (d.api.value !== current || definition.value !== reviewedDefinition)
      return;
    if (
      original.value &&
      result.current_revision !== original.value.revision_id
    ) {
      error.value =
        "The current revision changed while you were editing. Reload this screen and review the latest definition.";
      return;
    }
    plan.value = result;
  } catch (caught) {
    error.value = formatError(caught);
  } finally {
    busy.value = false;
  }
}
async function apply(): Promise<void> {
  confirmOpen.value = false;
  if (!plan.value) return;
  const operation = await d.mutate("/config/apply", {
    definition: definition.value,
    expected_revision: plan.value.current_revision,
  });
  if (operation)
    void router.push(`/operations/${encodeURIComponent(operation.id)}`);
}
</script>
<template>
  <RouterLink to="/jobs" class="back-link">← Jobs</RouterLink>
  <section class="page-heading">
    <div>
      <p class="eyebrow">VERSIONED CONFIGURATION</p>
      <h1>
        {{ original ? "Edit definition" : "Create workload"
        }}<span class="heading-dot">.</span>
      </h1>
      <p class="muted">
        Validate policy. Review the plan. Apply one immutable revision.
      </p>
    </div>
    <span class="source-badge">{{
      original?.source_kind.toUpperCase() ?? "API-OWNED"
    }}</span>
  </section>
  <p v-if="loading" role="status">Loading approved definition…</p>
  <div v-if="sourceOwned" class="message warning">
    <strong>File-owned definition: read only.</strong> Edit the operator-managed
    source file, then use the CLI config plan/apply workflow. This dashboard
    will not overwrite source ownership.
  </div>
  <div v-else-if="!editable" class="message warning">
    Definition editing requires workload.edit_safe_config. Executable, image,
    mount and privilege changes also require explicit elevated capabilities.
  </div>
  <div v-if="error" class="message danger" role="alert">{{ error }}</div>
  <div class="editor-layout">
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>{{ original ? original.name : "Workload definition" }}</h2>
          <p>YAML or canonical JSON · bounded to 64 KiB</p>
        </div>
        <label v-if="!original"
          ><span class="sr-only">Workload kind</span
          ><select v-model="kind" :disabled="!editable" @change="changeKind">
            <option>ScheduledJob</option>
            <option>OneShotJob</option>
            <option>Service</option>
          </select></label
        >
      </div>
      <label class="sr-only" for="definition">Workload YAML definition</label
      ><textarea
        id="definition"
        v-model="definition"
        class="definition-editor"
        spellcheck="false"
        autocomplete="off"
        :readonly="!editable"
        aria-describedby="definition-help"
      />
      <div class="panel-bottom">
        <span id="definition-help" class="hint"
          >No shell is implied. The server validates executable paths, argv,
          namespace and adapter policy.</span
        ><button
          class="primary"
          :disabled="
            !editable || busy || !d.connected.value || !definition.trim()
          "
          @click="review"
        >
          {{ busy ? "Validating…" : "Validate & review plan" }}
        </button>
      </div>
    </section>
    <aside class="editor-aside">
      <section class="panel">
        <div class="panel-heading"><h2>Execution authority</h2></div>
        <div class="panel-content">
          <p>
            Creating or changing a workload can grant executable authority on
            the host or configured adapter.
          </p>
          <ul class="plain-list">
            <li>Absolute executable + explicit argv</li>
            <li>Working directory and execution identity</li>
            <li>Docker images, mounts and networks</li>
            <li>systemd unit allowlist</li>
            <li>Timeout, retries, concurrency and log cap</li>
          </ul>
          <p class="hint">
            Review these fields in the definition before applying. An editor
            role alone does not grant arbitrary command creation.
          </p>
        </div>
      </section>
      <section v-if="validated && !plan" class="panel">
        <div class="panel-content">
          <p>Schema validated. A current plan is required before apply.</p>
        </div>
      </section>
      <section v-if="plan" class="panel plan-panel">
        <div class="panel-heading">
          <h2>Review proposed revision</h2>
          <span class="badge success">Validated</span>
        </div>
        <dl class="detail-grid">
          <div>
            <dt>Current revision</dt>
            <dd class="mono wrap">
              {{ plan.current_revision ?? "New workload" }}
            </dd>
          </div>
          <div>
            <dt>Proposed hash</dt>
            <dd class="mono wrap">{{ plan.proposed_hash }}</dd>
          </div>
          <div>
            <dt>Change</dt>
            <dd>
              {{
                plan.changed
                  ? "A new immutable revision will be applied."
                  : "No definition change detected."
              }}
            </dd>
          </div>
          <div>
            <dt>Ownership</dt>
            <dd>{{ plan.source_kind }}</dd>
          </div>
        </dl>
        <ul v-if="plan.warnings.length" class="plan-warnings">
          <li v-for="warning in plan.warnings" :key="warning">{{ warning }}</li>
        </ul>
        <div class="panel-content">
          <p class="hint">
            Apply uses the exact revision shown above. Acceptance creates an
            operation; it does not prove the job ran.
          </p>
          <button
            class="primary full"
            :disabled="
              !d.canAct.value ||
              !editable ||
              plan.source_kind.toLowerCase() !== 'api'
            "
            @click="confirmOpen = true"
          >
            Review impact & apply
          </button>
        </div>
      </section>
    </aside>
  </div>
  <section v-if="original" class="panel">
    <div class="panel-heading">
      <h2>Current approved definition</h2>
      <p>Compare with your proposed definition before applying.</p>
    </div>
    <pre class="code-block">{{ original.definition }}</pre>
  </section>
  <ConfirmDialog
    :open="confirmOpen"
    title="Apply this workload revision?"
    impact="This changes the approved executable configuration. Review command, arguments, identity, mounts, destination, timeout and retry policy above. Future executions use the new revision. An accepted apply is not proof of successful execution."
    confirm-label="Apply reviewed revision"
    @cancel="confirmOpen = false"
    @confirm="apply"
  />
</template>
