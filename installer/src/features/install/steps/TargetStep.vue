<script setup lang="ts">
import { useInstallStore, VERSION_LAUNCHERS } from '../../../app/stores/install';
import type { LauncherKind } from '../../../shared/types/installer';
import { LOADER_NAMES } from '../../../shared/types/installer';
import Alert from '../../../shared/ui/Alert.vue';
import Btn from '../../../shared/ui/Btn.vue';

const store = useInstallStore();

const isVersionLauncher = (kind: LauncherKind) => VERSION_LAUNCHERS.includes(kind);
</script>

<template>
  <div>
    <h2 class="text-lg font-bold text-primary">Выберите инстанс</h2>
    <p class="mt-1 text-sm text-secondary">
      <template v-if="store.pack?.is_modded">
        Подходят только инстансы той же версии и загрузчика — остальные скрыты, чтобы не сломать игру.
      </template>
      <template v-else>
        Ванильный мир живёт в любом инстансе подходящей версии.
      </template>
    </p>

    <Alert v-if="store.loaderVersionAuto && store.loaderVersion" tone="info" class="mt-3">
      Версия загрузчика подобрана автоматически: {{ store.loaderVersion }}. Её можно изменить позже в лаунчере.
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
          {{ t.minecraft_version }} · {{ LOADER_NAMES[t.loader] }}{{ t.loader_version ? ` ${t.loader_version}` : '' }}
        </p>
      </button>

      <button
        v-if="store.launcher?.kind !== 'manual'"
        type="button"
        class="w-full rounded-xl border border-dashed border-primary-500/40 p-4 text-left transition-colors hover:border-primary-400 cursor-pointer"
        :disabled="store.busy"
        @click="store.createNewTarget()"
      >
        <template v-if="isVersionLauncher(store.launcher?.kind ?? 'manual') && store.pack?.is_modded">
          <p class="text-sm font-semibold text-primary-400">
            Установить версию «{{ store.pack?.world_version }} · {{ store.pack ? LOADER_NAMES[store.pack.loader] : '' }}»
          </p>
          <p class="mt-1 text-xs text-secondary">
            Скачаем официальный установщик и подготовим версию с пометкой [KK]. Ничего нажимать не нужно.
            Если в общих mods/ есть моды, они будут заменены модами карты (старые сохраним в бэкап).
          </p>
        </template>
        <template v-else-if="store.launcher?.kind === 'official'">
          <p class="text-sm font-semibold text-primary-400">
            Создать установку «{{ store.pack?.world_name }}»
          </p>
          <p class="mt-1 text-xs text-secondary">
            Своя папка игры под карту — ваши установки и миры не пострадают.
            После установки мир появится в панели быстрого запуска.
          </p>
        </template>
        <template v-else>
          <p class="text-sm font-semibold text-primary-400">
            Создать новый инстанс «{{ store.pack?.world_name }}»
          </p>
          <p class="mt-1 text-xs text-secondary">Рекомендуется для модовых карт — ничего чужого не тронем.</p>
        </template>
      </button>
    </div>

    <div class="mt-4">
      <Btn variant="secondary" @click="store.step = 'launcher'">Назад</Btn>
    </div>
  </div>
</template>
