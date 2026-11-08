<script setup lang="ts">
import { computed } from "vue";
import { dashboard as d } from "../store";
import { isService, time } from "../format";
import StateBadge from "../components/StateBadge.vue";
import AdapterHealth from "../components/AdapterHealth.vue";
import ExecutionTable from "../components/ExecutionTable.vue";
const failures = computed(() =>
  d.executions.value.filter((item) =>
    ["failed", "timed_out", "unknown"].includes(item.state.toLowerCase()),
  ),
);
const nextDue = computed(() =>
  d.workloads.value
    .filter((item) => !isService(item.kind) && item.next_due_at_us !== null)
    .sort(
      (a, b) => (a.next_due_at_us ?? Infinity) - (b.next_due_at_us ?? Infinity),
    )
    .slice(0, 5),
);
const services = computed(() =>
  d.workloads.value.filter((item) => isService(item.kind)),
);
const healthy = computed(
  () =>
    services.value.filter((item) =>
      ["ready"].includes(item.observed_state?.toLowerCase() ?? ""),
    ).length,
);
const attention = computed(
  () => failures.value.length + (d.status.value?.restore_required ? 1 : 0),
);
</script>
<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">CONTROL PLANE</p>
      <h1>Overview<span class="heading-dot">.</span></h1>
      <p class="muted">
        What needs attention, what’s running, and what’s next.
      </p>
    </div>
    <RouterLink
      v-if="d.can('workload.edit_safe_config')"
      to="/jobs/new"
      class="button primary"
    >
      <span aria-hidden="true">+</span> Create workload</RouterLink
    >
  </section>
  <div v-if="d.status.value?.restore_required" class="message warning">
    <strong>Restore quarantine is active.</strong> Review external side effects
    and resume through the trusted CLI with explicit evidence. Automatic
    dispatch is blocked.
  </div>
  <div v-if="d.status.value?.paused" class="message warning">
    <strong>Global dispatch is paused.</strong> Workload schedules remain
    visible. Resume through the trusted administrative CLI.
  </div>
  <div class="metrics-grid">
    <section class="metric">
      <div class="metric-label">
        Configured workloads<svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z" />
        </svg>
      </div>
      <strong>{{ d.status.value?.workloads ?? "—" }}</strong
      ><span class="metric-note"
        >{{ d.workloads.value.filter((w) => w.paused).length }} paused in this
        page</span
      >
    </section>
    <section class="metric">
      <div class="metric-label">
        Active execution slots<svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="m9 5 11 7-11 7zM4 4v16" />
        </svg>
      </div>
      <strong>{{ d.status.value?.active ?? "—" }}</strong
      ><span class="metric-note"
        ><span class="status-dot" aria-hidden="true" /> Observed by the
        daemon</span
      >
    </section>
    <section class="metric" :class="{ 'attention-metric': attention }">
      <div class="metric-label">
        Needs attention<svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="m12 3 10 18H2zM12 9v5M12 17v1" />
        </svg>
      </div>
      <strong>{{ attention }}</strong
      ><span class="metric-note"
        >{{ d.status.value?.unknown ?? 0 }} uncertain outcomes
        instance-wide</span
      >
    </section>
    <section class="metric">
      <div class="metric-label">
        Services observed ready<svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M3 5h18v6H3zM3 15h18v6H3zM7 8h.1M7 18h.1" />
        </svg>
      </div>
      <strong
        >{{ healthy
        }}<span class="metric-denominator">
          / {{ services.length }}</span
        ></strong
      ><span class="metric-note"
        >Desired and observed states tracked separately</span
      >
    </section>
  </div>
  <section
    v-if="!d.workloads.value.length"
    class="empty-state panel first-empty"
  >
    <div class="empty-symbol" aria-hidden="true">
      <svg viewBox="0 0 24 24">
        <path d="M3 6h18v15H3zM7 3v6M17 3v6M3 11h18M9 16h6" />
      </svg>
    </div>
    <p class="eyebrow">YOUR FIRST WORKLOAD</p>
    <h2>A quiet harbor. Let’s schedule something.</h2>
    <p>
      Define an API-owned workload, validate its execution policy, and review
      the proposed revision before applying.
    </p>
    <RouterLink
      v-if="d.can('workload.edit_safe_config')"
      class="button primary"
      to="/jobs/new"
      >Create first workload <span aria-hidden="true">→</span></RouterLink
    >
  </section>
  <section class="panel">
    <div class="panel-heading">
      <div>
        <h2>
          Attention queue <span class="count">{{ failures.length }}</span>
        </h2>
        <p>Recent failed and uncertain executions in the loaded page.</p>
      </div>
      <RouterLink to="/executions" class="text-link"
        >All executions <span aria-hidden="true">→</span></RouterLink
      >
    </div>
    <ExecutionTable v-if="failures.length" :items="failures.slice(0, 5)" />
    <div v-else-if="!d.can('execution.read')" class="inline-empty">
      Execution evidence requires execution.read. The attention queue is
      unavailable.
    </div>
    <div v-else class="inline-empty">
      <span class="success-mark" aria-hidden="true">✓</span>
      <div>
        <strong>No failed or unknown executions in this page</strong>
        <p>
          The daemon remains the source of truth. Refresh to verify current
          state.
        </p>
      </div>
    </div>
  </section>
  <div class="two-column">
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>Next on the schedule</h2>
          <p>Upcoming due times · your local timezone</p>
        </div>
        <RouterLink to="/jobs" class="text-link">View jobs →</RouterLink>
      </div>
      <div v-if="!nextDue.length" class="inline-empty">
        <p>No scheduled workloads with a next due time.</p>
      </div>
      <RouterLink
        v-for="workload in nextDue"
        :key="workload.id"
        :to="`/jobs/${encodeURIComponent(workload.id)}`"
        class="schedule-row"
        ><span class="schedule-icon" aria-hidden="true"
          ><svg viewBox="0 0 24 24">
            <circle cx="12" cy="12" r="8" />
            <path d="M12 7v5l3 2" /></svg
        ></span>
        <div>
          <strong>{{ workload.name }}</strong
          ><small
            >{{ workload.namespace }} ·
            {{ workload.schedule_description }}</small
          >
        </div>
        <span>{{ time(workload.next_due_at_us) }}</span></RouterLink
      >
    </section>
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>Service health</h2>
          <p>Desired state ≠ observed readiness</p>
        </div>
        <RouterLink to="/services" class="text-link"
          >View services →</RouterLink
        >
      </div>
      <div v-if="!services.length" class="inline-empty">
        <p>No continuous services configured.</p>
      </div>
      <RouterLink
        v-for="service in services.slice(0, 5)"
        :key="service.id"
        :to="`/services/${encodeURIComponent(service.id)}`"
        class="service-row"
        ><div>
          <strong>{{ service.name }}</strong
          ><small
            >Desired {{ service.desired_state ?? "unavailable" }} ·
            {{ service.restart_count }} restarts</small
          >
        </div>
        <StateBadge :state="service.observed_state ?? 'unknown'"
      /></RouterLink>
    </section>
  </div>
  <AdapterHealth />
  <section class="status-strip">
    <div>
      <span
        class="status-dot"
        :class="{ disconnected: !d.connected.value }"
        aria-hidden="true"
      /><strong
        >Scheduler
        {{
          d.status.value?.admission?.paused
            ? "paused by storage budget"
            : d.status.value?.paused
              ? "paused"
            : d.connected.value
              ? "connected"
              : "unavailable"
        }}</strong
      >
    </div>
    <span>SQLite {{ d.status.value?.sqlite_version ?? "unavailable" }}</span
    ><span>Single active owner</span
    ><RouterLink to="/settings" class="text-link"
      >Instance & permissions →</RouterLink
    >
  </section>
</template>
