<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore } from '../../../app/stores/install';

const store = useInstallStore();

const hint = computed(() =>
  store.compatBranch === 'optifine'
    ? 'Подбираем подходящую версию Forge и OptiFine. Обычно это занимает несколько секунд.'
    : 'Подбираем сборку Fabulously Optimized на Fabric как современная альтернатива OptiFine. Обычно это занимает несколько секунд.',
);
</script>

<template>
  <div class="flex flex-col items-center py-10 text-center">
    <svg
      class="animate-spin h-10 w-10 text-primary-400"
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <circle class="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" stroke-width="4" />
      <path
        class="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
      />
    </svg>
    <h2 class="mt-5 text-lg font-bold text-primary">Готовим сборку совместимости…</h2>
    <p class="mt-2 max-w-md text-sm text-secondary">{{ hint }}</p>
  </div>
</template>
