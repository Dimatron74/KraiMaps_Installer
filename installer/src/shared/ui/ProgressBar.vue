<script setup lang="ts">
import { computed } from 'vue';

const props = defineProps<{
  /** 0..100. `null` — объём неизвестен: бегущая indeterminate-анимация. */
  percent: number | null;
  label: string;
  currentFile?: string;
}>();

const clamped = computed(() =>
  props.percent === null ? null : Math.min(100, Math.max(0, props.percent)),
);
</script>

<template>
  <div>
    <div class="flex items-center justify-between text-xs text-secondary">
      <span>{{ label }}</span>
      <span v-if="clamped !== null" class="font-mono">{{ Math.round(clamped) }}%</span>
    </div>
    <div
      class="mt-2 h-2 overflow-hidden rounded-full bg-white/10"
      role="progressbar"
      :aria-valuenow="clamped === null ? undefined : Math.round(clamped)"
      aria-valuemin="0"
      aria-valuemax="100"
    >
      <!-- Быстрый линейный переход: события идут десятками в секунду,
        долгая анимация вечно догоняла бы и полоса стояла бы на месте. -->
      <div
        v-if="clamped !== null"
        class="h-full rounded-full bg-primary-500 transition-[width] duration-100 ease-linear"
        :style="{ width: `${clamped}%` }"
      />
      <div v-else class="mi-indeterminate h-full rounded-full bg-primary-500" aria-hidden="true" />
    </div>
    <p v-if="currentFile" class="mt-1 truncate font-mono text-[11px] text-tertiary">
      {{ currentFile }}
    </p>
    <!-- Слот сразу под строкой статуса: для ссылок, которые должны выглядеть
      частью этой строки, а не отдельным элементом под полосой. -->
    <slot name="after" />
  </div>
</template>

<style scoped>
/* Неопределённый прогресс: светящийся отрезок бежит по треку. */
.mi-indeterminate {
  width: 35%;
  animation: mi-slide 1.2s ease-in-out infinite;
  box-shadow: 0 0 12px rgba(14, 165, 233, 0.7);
}

@keyframes mi-slide {
  0% {
    margin-left: -35%;
  }
  100% {
    margin-left: 100%;
  }
}

@media (prefers-reduced-motion: reduce) {
  .mi-indeterminate {
    animation: none;
    width: 100%;
    opacity: 0.5;
  }
}
</style>
