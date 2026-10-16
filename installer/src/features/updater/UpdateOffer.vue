<!--
  Мягкий оффер обновления (soft): немодальное окно поверх контента.
  Отказ — кнопка «Позже» (запоминаем версию, больше авто не предлагаем).
-->
<script setup lang="ts">
import { computed } from 'vue';
import { useUpdaterStore } from '../../app/stores/updater';
import Alert from '../../shared/ui/Alert.vue';
import Btn from '../../shared/ui/Btn.vue';
import ErrorDetails from '../../shared/ui/ErrorDetails.vue';
import ProgressBar from '../../shared/ui/ProgressBar.vue';

const updater = useUpdaterStore();

const busy = computed(() => updater.phase === 'checking' || updater.phase === 'downloading');
const versionLine = computed(() =>
  updater.latest ? `${updater.version} → ${updater.latest}` : updater.version,
);
</script>

<template>
  <div
    class="fixed inset-0 z-40 flex items-center justify-center bg-black/60 p-6"
    role="dialog"
    aria-modal="true"
    aria-label="Доступно обновление"
  >
    <div class="glass-card w-full max-w-md p-6">
      <p class="text-xs font-semibold uppercase tracking-[0.2em] text-emerald-300/80">
        Обновление
      </p>
      <h2 class="mt-2 text-xl font-extrabold text-primary">Вышла новая версия</h2>
      <p class="mt-1 font-mono text-xs text-tertiary">{{ versionLine }}</p>

      <p
        v-if="updater.notes"
        class="mt-3 max-h-40 overflow-y-auto whitespace-pre-line text-sm leading-relaxed text-secondary"
      >
        {{ updater.notes }}
      </p>
      <p v-else class="mt-3 text-sm leading-relaxed text-secondary">
        Рекомендуем обновиться: исправления и улучшения. Обновление займёт меньше минуты.
      </p>

      <div v-if="updater.phase === 'downloading'" class="mt-4">
        <ProgressBar :percent="updater.percent" label="Скачиваем обновление…" />
      </div>

      <Alert v-if="updater.phase === 'error' && updater.error" tone="error" class="mt-4 text-left">
        <p class="text-sm font-semibold text-primary">{{ updater.error.message }}</p>
        <ErrorDetails
          class="mt-2"
          :code="updater.error.code"
          :message="updater.error.message"
          :details="updater.error.details"
          hint="Проверьте интернет и попробуйте снова. Если подпись не сошлась — скачайте установщик вручную с сайта."
        />
      </Alert>

      <div class="mt-5 flex gap-3">
        <Btn
          class="flex-1"
          :loading="busy"
          :disabled="busy"
          @click="updater.startUpdate()"
        >
          {{ updater.phase === 'downloading' ? 'Обновляем…' : 'Обновить' }}
        </Btn>
        <Btn
          variant="secondary"
          :disabled="busy"
          @click="updater.dismissOffer()"
        >
          Позже
        </Btn>
      </div>
      <p v-if="updater.manualMessage" class="mt-3 text-xs text-tertiary">
        {{ updater.manualMessage }}
      </p>
    </div>
  </div>
</template>
