<script setup lang="ts">
import { computed } from 'vue';
import type { CompatFailure, CompatReport, CompatSummary } from '../../../shared/types/installer';
import { LOADER_NAMES } from '../../../shared/types/installer';

const props = defineProps<{
  summary: CompatSummary | null;
  report?: CompatReport | null;
  failure?: CompatFailure | null;
}>();

const error = computed(() => props.report?.error ?? props.failure);
</script>

<template>
  <details v-if="summary || report || error" class="mt-2 text-left">
    <summary
      class="cursor-pointer text-xs font-medium text-primary-400 transition-colors hover:text-primary-300"
    >
      Подробнее
    </summary>
    <div class="mt-2 rounded-lg bg-black/30 p-3 font-mono text-[11px] text-tertiary wrap-anywhere">
      <template v-if="summary">
        <p>Minecraft: {{ summary.mc_version }}</p>
        <p>Загрузчик: {{ LOADER_NAMES[summary.loader] }} {{ summary.loader_version }}</p>
        <p>{{ summary.kind === 'fo' ? 'Версия сборки' : 'Файл OptiFine' }}: {{ summary.version }}</p>
      </template>
      <p v-if="report?.jar_sha256">SHA-256: {{ report.jar_sha256 }}</p>
      <template v-if="error">
        <p class="mt-2">Код: {{ error.code }}</p>
        <pre v-if="error.details" class="mt-1 whitespace-pre-wrap font-mono">{{ error.details }}</pre>
      </template>
    </div>
  </details>
</template>
