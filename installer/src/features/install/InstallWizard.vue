<script setup lang="ts">
import { useInstallStore } from '../../app/stores/install';
import ConfirmStep from './steps/ConfirmStep.vue';
import ConflictStep from './steps/ConflictStep.vue';
import DeployStep from './steps/DeployStep.vue';
import DoneStep from './steps/DoneStep.vue';
import ErrorStep from './steps/ErrorStep.vue';
import FileStep from './steps/FileStep.vue';
import LauncherStep from './steps/LauncherStep.vue';
import TargetStep from './steps/TargetStep.vue';
import WipeStep from './steps/WipeStep.vue';

const store = useInstallStore();

const steps = [
  { id: 'file', label: 'Файл' },
  { id: 'confirm', label: 'Проверка' },
  { id: 'launcher', label: 'Лаунчер' },
  { id: 'target', label: 'Инстанс' },
  { id: 'deploying', label: 'Установка' },
  { id: 'done', label: 'Готово' },
] as const;

const stepIndex: Record<string, number> = {
  file: 0,
  analyzing: 0,
  confirm: 1,
  launcher: 2,
  target: 3,
  wipe: 3,
  conflict: 3,
  deploying: 4,
  done: 5,
  error: -1,
};
</script>

<template>
  <div class="flex flex-1 flex-col">
    <!-- Индикатор шагов: всегда сверху -->
    <ol v-if="(stepIndex[store.step] ?? -1) >= 0" class="mb-6 flex items-center gap-1.5" aria-label="Шаги установки">
      <li v-for="(s, i) in steps" :key="s.id" class="flex flex-1 items-center gap-1.5 last:flex-none">
        <span
          :class="[
            'flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[11px] font-bold',
            i < (stepIndex[store.step] ?? 0)
              ? 'bg-success-500/20 text-success-400'
              : i === (stepIndex[store.step] ?? 0)
                ? 'bg-primary-500 text-white'
                : 'bg-white/5 text-tertiary',
          ]"
        >
          {{ i + 1 }}
        </span>
        <span
          :class="[
            'hidden text-xs sm:inline',
            i <= (stepIndex[store.step] ?? 0) ? 'text-secondary' : 'text-disabled',
          ]"
        >
          {{ s.label }}
        </span>
        <span v-if="i < steps.length - 1" class="h-px flex-1 bg-white/10" />
      </li>
    </ol>

    <!-- Контент шага: по центру оставшейся высоты -->
    <div class="flex flex-1 flex-col">
      <div class="m-auto w-full">
        <FileStep v-if="store.step === 'file' || store.step === 'analyzing'" />
        <ConfirmStep v-else-if="store.step === 'confirm'" />
        <LauncherStep v-else-if="store.step === 'launcher'" />
        <TargetStep v-else-if="store.step === 'target'" />
        <WipeStep v-else-if="store.step === 'wipe'" />
        <ConflictStep v-else-if="store.step === 'conflict'" />
        <DeployStep v-else-if="store.step === 'deploying'" />
        <DoneStep v-else-if="store.step === 'done'" />
        <ErrorStep v-else-if="store.step === 'error'" />
      </div>
    </div>
  </div>
</template>
