<script setup lang="ts">
defineProps<{ show: boolean }>();
</script>

<template>
  <Transition name="drop-overlay">
    <div
      v-if="show"
      class="pointer-events-none fixed inset-0 z-30 flex items-center justify-center bg-[#050810]/70 backdrop-blur-sm"
      aria-hidden="true"
    >
      <div
        class="mx-6 flex w-full max-w-md flex-col items-center rounded-2xl border-2 border-dashed border-primary-400/60 bg-white/[0.03] px-8 py-12 text-center shadow-[0_0_80px_rgba(56,189,248,0.18)]"
      >
        <div
          class="flex h-16 w-16 items-center justify-center rounded-2xl border border-primary-400/30 bg-primary-500/10"
        >
          <svg
            class="h-7 w-7 text-primary-300"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M12 15V3m0 0 4 4m-4-4-4 4" />
            <path d="M4 15v3a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-3" />
          </svg>
        </div>
        <p class="mt-5 text-lg font-bold text-primary">Отпустите для установки карты</p>
        <p class="mt-1.5 text-sm text-secondary">Принимается один zip-архив карты</p>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.drop-overlay-enter-active,
.drop-overlay-leave-active {
  transition: opacity 0.12s ease;
}

.drop-overlay-enter-from,
.drop-overlay-leave-to {
  opacity: 0;
}
</style>
