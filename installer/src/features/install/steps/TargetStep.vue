<script setup lang="ts">
import { computed } from 'vue';
import { useInstallStore } from '../../../app/stores/install';
import { LAUNCHER_NAMES, LOADER_NAMES } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();

/**
 * Версия, которая уже есть на диске: «1.21.11» для ваниллы,
 * «1.21.11 · Fabric 0.16.9» для модовой.
 */
const takenLabel = computed(() => {
  const t = store.takenVersionTarget;
  if (!t) return '';
  const loader = LOADER_NAMES[t.loader];
  return t.loader_version
    ? `${t.minecraft_version} · ${loader} ${t.loader_version}`
    : t.minecraft_version;
});
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">Выберите инстанс</h2>
    <p class="mt-1 text-sm text-secondary">
      <template v-if="store.pack?.is_modded">
        Подходят только инстансы той же версии и загрузчика — остальные скрыты, чтобы не сломать
        игру.
      </template>
      <template v-else> Ванильный мир живёт в любом инстансе подходящей версии. </template>
    </p>

    <Alert v-if="store.loaderVersionAuto && store.loaderVersion" tone="info" class="mt-3">
      Версия загрузчика подобрана автоматически: {{ store.loaderVersion }}. Её можно изменить позже
      в лаунчере.
    </Alert>

    <div class="mt-4 space-y-2">
      <button
        v-for="t in store.suitableTargets"
        :key="t.launcher + t.path"
        type="button"
        class="glass-card w-full p-4 text-left transition-colors hover:border-white/20 cursor-pointer"
        @click="store.selectTarget(t)"
      >
        <p class="text-sm font-semibold text-primary">{{ t.name }}</p>
        <p class="mt-1 text-xs text-secondary">
          {{ t.minecraft_version }} · {{ LOADER_NAMES[t.loader]
          }}{{ t.loader_version ? ` ${t.loader_version}` : '' }}
        </p>
      </button>

      <Alert v-if="store.takenVersionTarget" tone="info">
        Версия «{{ takenLabel }}» уже есть в
        {{ LAUNCHER_NAMES[store.takenVersionTarget.launcher] }} — вторая такая папка невозможна.
        Выберите её в списке выше: карта установится туда.
      </Alert>

      <button
        v-else-if="store.newTargetCta"
        type="button"
        class="w-full rounded-xl border border-dashed border-primary-500/40 p-4 text-left transition-colors hover:border-primary-400 cursor-pointer"
        :disabled="store.busy"
        @click="store.createNewTarget()"
      >
        <p class="text-sm font-semibold text-primary-400">{{ store.newTargetCta.title }}</p>
        <p class="mt-1 text-xs text-secondary">{{ store.newTargetCta.hint }}</p>
      </button>
    </div>

    <div class="mt-4">
      <Btn variant="secondary" @click="store.step = 'launcher'">Назад</Btn>
    </div>
  </div>
</template>
