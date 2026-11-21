import type { KeyboardEvent, MouseEvent } from 'react';

import { InlineName } from '../../components/InlineName/InlineName';
import { openMenuAt, openMenuBelow } from '../../components/Menu/menuStore';
import { ExportIcon } from '../../components/icons/ExportIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { ProjectIcon } from '../../components/icons/ProjectIcon';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { ja } from '../../i18n/ja';
import type { ListView } from '../../ipc/items';
import type { ProjectDto } from '../../ipc/projects';
import { formatCount } from '../../lib/format';
import { useViewStore } from '../../stores/viewStore';
import {
  addProjectAction,
  projectMenu,
  removeProjectWithConfirm,
  renameProjectTo,
} from '../projects/projectActions';
import { useProjectStore } from '../projects/projectStore';
import { ROW_ATTR } from './Sidebar';
import styles from './Sidebar.module.css';

const t = ja.projects;

/**
 * サイドバーのプロジェクト（M15 段 6。design.md §3.1）。行を押すとそのプロジェクトで今使っている素材の一覧、右クリック
 * （⌃↩）で持ち出し先・持ち出しの設定・クレジットの一覧・照らし直す・場所・Finder・名前・登録を外す。＋ でフォルダを選んで
 * 登録する。フォルダが見つからなければ注意の印、持ち出し先には書き出しの印。
 */
export function ProjectList({ current }: { current: ListView | null }) {
  const projects = useProjectStore((s) => s.list);
  const target = useProjectStore((s) => s.target);
  const renaming = useProjectStore((s) => s.renaming);
  const setRenaming = useProjectStore((s) => s.setRenaming);
  const setView = useViewStore((s) => s.setView);

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, project: ProjectDto) => {
    const plain = !e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey;
    let handled = true;
    if (e.key === 'Enter' && plain) {
      setRenaming(project.id);
    } else if (e.key === 'Enter' && e.ctrlKey) {
      openMenuBelow(
        e.currentTarget,
        t.label(project.name),
        projectMenu(project, () => setRenaming(project.id)),
      );
    } else if (e.key === 'Backspace' && e.metaKey) {
      void removeProjectWithConfirm(project);
    } else {
      handled = false;
    }
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const onContextMenu = (e: MouseEvent, project: ProjectDto) => {
    e.preventDefault();
    openMenuAt(
      e.clientX,
      e.clientY,
      t.label(project.name),
      projectMenu(project, () => setRenaming(project.id)),
    );
  };

  return (
    <section className={styles.section} aria-label={ja.sidebar.projects}>
      <div className={styles.sectionHeader}>
        <h2 className={styles.sectionTitle}>{ja.sidebar.projects}</h2>
        <button
          type="button"
          className={styles.addButton}
          aria-label={ja.sidebar.addProject}
          title={ja.sidebar.addProject}
          onClick={() => void addProjectAction()}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </div>
      {projects.map((project) => {
        const selected = current?.kind === 'project' && current.id === project.id;
        const isTarget = target?.effective === project.id;
        // 選んでいないが登録が 1 つなので持ち出し先（ADR-207 (1)）
        const implicit = isTarget && target?.chosen !== project.id;
        if (renaming === project.id) {
          return (
            <div key={project.id} className={styles.row}>
              <span className={styles.rowIcon}>
                <ProjectIcon size={16} strokeWidth={1.8} />
              </span>
              <InlineName
                initial={project.name}
                label={ja.sidebar.projectName}
                onCommit={(name) => {
                  setRenaming(null);
                  void renameProjectTo(project.id, name);
                }}
                onCancel={() => setRenaming(null)}
              />
            </div>
          );
        }
        return (
          <button
            key={project.id}
            type="button"
            className={styles.row}
            aria-current={selected ? 'page' : undefined}
            title={`${project.root}（${t.engines[project.engine]}）`}
            {...{ [ROW_ATTR]: '' }}
            onClick={(e) => {
              e.currentTarget.focus();
              if (!selected) setView({ kind: 'project', id: project.id });
            }}
            onKeyDown={(e) => onKeyDown(e, project)}
            onContextMenu={(e) => onContextMenu(e, project)}
          >
            <span className={styles.rowIcon}>
              <ProjectIcon size={16} strokeWidth={1.8} />
            </span>
            <span className={styles.rowLabel}>{project.name}</span>
            {project.missing && (
              <span
                className={styles.rowMark}
                data-kind="missing"
                role="img"
                aria-label={t.missing}
              >
                <WarningIcon size={13} strokeWidth={2} />
              </span>
            )}
            {isTarget && (
              <span
                className={styles.rowMark}
                data-kind="target"
                role="img"
                aria-label={implicit ? t.implicitTarget : t.target}
                title={implicit ? t.implicitTarget : t.target}
              >
                <ExportIcon size={13} strokeWidth={2} />
              </span>
            )}
            <span className={styles.count}>{formatCount(project.items)}</span>
          </button>
        );
      })}
    </section>
  );
}
