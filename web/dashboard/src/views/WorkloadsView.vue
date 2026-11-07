<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { dashboard as d } from "../store";
import { isService, time } from "../format";
import StateBadge from "../components/StateBadge.vue";
const props = defineProps<{ kind: "jobs" | "services" }>();
const tableWrap = ref<HTMLElement>();
const scrollTop = ref(0);
const rowHeight = 80;
const query = ref("");
const namespace = ref("");
const state = ref("");
const all = computed(() =>
  d.workloads.value.filter(
    (w) => isService(w.kind) === (props.kind === "services"),
  ),
);
const namespaces = computed(() =>
  [...new Set(all.value.map((w) => w.namespace))].sort(),
);
const items = computed(() =>
  all.value.filter(
    (w) =>
      (!query.value ||
        `${w.name} ${w.namespace} ${w.id}`
          .toLowerCase()
          .includes(query.value.toLowerCase())) &&
      (!namespace.value || w.namespace === namespace.value) &&
      (!state.value ||
        (state.value === "paused" ? w.paused : !w.paused && w.enabled)),
  ),
);
const virtual = computed(() => items.value.length > 50);
const start = computed(() =>
  virtual.value ? Math.max(0, Math.floor(scrollTop.value / rowHeight) - 4) : 0,
);
const visible = computed(() =>
  virtual.value
    ? items.value.slice(start.value, start.value + 20)
    : items.value,
);
watch([query, namespace, state], () => {
  scrollTop.value = 0;
  if (tableWrap.value) tableWrap.value.scrollTop = 0;
});
function scroll(event: Event): void {
  if (event.currentTarget instanceof HTMLElement)
    scrollTop.value = event.currentTarget.scrollTop;
}
</script>
<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">WORKLOADS</p>
      <h1>
        {{ kind === "jobs" ? "Jobs" : "Services"
        }}<span class="heading-dot">.</span>
      </h1>
      <p class="muted">
        {{
          kind === "jobs"
            ? "Scheduled and one-shot work. Every occurrence has an owner."
            : "Continuous workloads. Desired state, observed state, explicit control."
        }}
      </p>
    </div>
    <RouterLink
      v-if="d.can('workload.edit_safe_config')"
      class="button primary"
      :to="`/jobs/new${kind === 'services' ? '?kind=Service' : ''}`"
      >+ Create {{ kind === "jobs" ? "job" : "service" }}</RouterLink
    >
  </section>
  <p v-if="!d.can('workload.read')" class="message warning">
    Workload visibility requires workload.read. No workload data is loaded for
    this token.
  </p>
  <section v-else class="panel">
    <div class="filter-bar">
      <label class="search-field"
        ><svg viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="10" cy="10" r="6" />
          <path d="m15 15 6 6" /></svg
        ><span class="sr-only">Search {{ kind }}</span
        ><input
          v-model="query"
          :placeholder="`Search ${kind} by name or namespace…`"
          type="search" /></label
      ><label
        ><span class="sr-only">Namespace</span
        ><select v-model="namespace">
          <option value="">All namespaces</option>
          <option v-for="ns in namespaces" :key="ns">{{ ns }}</option>
        </select></label
      ><label
        ><span class="sr-only">Workload state</span
        ><select v-model="state">
          <option value="">All states</option>
          <option value="enabled">Enabled</option>
          <option value="paused">Paused</option>
        </select></label
      ><span class="muted result-count">{{ items.length }} {{ kind }}</span>
    </div>
    <div v-if="!items.length" class="empty-state">
      <h2>{{ all.length ? "No matching workloads" : `No ${kind} yet` }}</h2>
      <p>
        {{
          all.length
            ? "Try a different search or filter."
            : "Create a definition, validate policy, then review the proposed revision."
        }}
      </p>
      <RouterLink
        v-if="!all.length && d.can('workload.edit_safe_config')"
        class="button primary"
        to="/jobs/new"
        >Create first workload</RouterLink
      >
    </div>
    <div
      v-else
      ref="tableWrap"
      class="table-wrap"
      :class="{ 'virtual-table': virtual }"
      @scroll="scroll"
    >
      <table>
        <thead>
          <tr>
            <th>Name / namespace</th>
            <th>{{ kind === "jobs" ? "Schedule" : "Desired / observed" }}</th>
            <th>Ownership</th>
            <th>{{ kind === "jobs" ? "Next due · local" : "Restarts" }}</th>
            <th>State</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="virtual && start" class="spacer" aria-hidden="true">
            <td colspan="5" :style="{ height: `${start * rowHeight}px` }" />
          </tr>
          <tr v-for="workload in visible" :key="workload.id">
            <td>
              <RouterLink
                class="strong"
                :to="`/${kind}/${encodeURIComponent(workload.id)}`"
                >{{ workload.name }}</RouterLink
              ><small>{{ workload.namespace }} · {{ workload.executor }}</small>
            </td>
            <td v-if="kind === 'jobs'">
              <span class="mono">{{
                workload.schedule_description ?? "Manual · one-shot"
              }}</span>
            </td>
            <td v-else>
              <span>{{ workload.desired_state ?? "unavailable" }}</span
              ><small
                >Observed {{ workload.observed_state ?? "unknown" }}</small
              >
            </td>
            <td>
              <span class="source-badge">{{
                workload.source_kind.toUpperCase()
              }}</span>
            </td>
            <td>
              {{
                kind === "jobs"
                  ? time(workload.next_due_at_us)
                  : workload.restart_count
              }}
            </td>
            <td>
              <StateBadge
                :state="
                  workload.paused
                    ? 'paused'
                    : kind === 'services'
                      ? (workload.observed_state ?? 'unknown')
                      : workload.enabled
                        ? 'enabled'
                        : 'disabled'
                "
              />
            </td>
          </tr>
          <tr
            v-if="virtual && items.length > start + visible.length"
            class="spacer"
            aria-hidden="true"
          >
            <td
              colspan="5"
              :style="{
                height: `${(items.length - start - visible.length) * rowHeight}px`,
              }"
            />
          </tr>
        </tbody>
      </table>
    </div>
    <div v-if="d.workloadCursor.value" class="panel-bottom">
      <button @click="d.loadMore('workloads')">Load next page</button
      ><span class="muted"
        >Results are bounded by the API. Filters apply to loaded pages.</span
      >
    </div>
  </section>
</template>
