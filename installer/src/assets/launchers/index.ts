/**
 * Иконки лаунчеров (256px webp) — те же файлы, что на сайте в
 * kraimaps-site/frontend/public/launchers, чтобы пользователь узнавал те же
 * логотипы, что и на странице установщика.
 *
 * Импортом, а не из `public/`: Vite хэширует файлы и кладёт их рядом с
 * бандлом, поэтому путь не зависит от base и не ломается в сборке под
 * NSIS. Импорт же даёт проверку типов и мгновенное подсказывание при
 * переименовании файла.
 *
 * `manual` — свой SVG: на сайте папки вручную нет, а лаунчером она не
 * является, но плитка в списке должна быть у каждой карточки.
 */
import legacyIcon from './legacy.webp';
import manualIcon from './manual.svg';
import minecraftIcon from './minecraft.webp';
import modrinthIcon from './modrinth.webp';
import multimcIcon from './multimc.webp';
import pineconeIcon from './pinecone.webp';
import polymcIcon from './polymc.webp';
import prismIcon from './prism.webp';
import tlauncherIcon from './tlauncher.webp';
import type { LauncherKind } from '../../shared/types/installer';

export const LAUNCHER_ICONS: Record<LauncherKind, string> = {
  prism: prismIcon,
  pinecone: pineconeIcon,
  polymc: polymcIcon,
  multimc: multimcIcon,
  modrinth: modrinthIcon,
  manual: manualIcon,
  tlauncher: tlauncherIcon,
  legacy: legacyIcon,
  official: minecraftIcon,
};
