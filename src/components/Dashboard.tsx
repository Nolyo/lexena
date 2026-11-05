"use client";

import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { DashboardHeader } from "./common/DashboardHeader";
import {
  DashboardSidebar,
  type DashboardTabId,
} from "./dashboard/DashboardSidebar";
import { AccueilTab } from "./dashboard/tabs/AccueilTab";
import { HistoriqueTab } from "./dashboard/tabs/HistoriqueTab";
import { StatistiquesTab } from "./dashboard/tabs/StatistiquesTab";
import { SettingTabs } from "./settings/SettingTabs";
import {
  type SettingsSectionId,
  resolveSettingsTarget,
  scrollToSettingsAnchor,
} from "./settings/common/SettingsNav";
import {
  ALL_LEVELS_ON,
  LogsTab,
  type LevelFilter,
} from "./logs/LogsTab";
import { NotesEditor } from "./notes/NotesEditor/NotesEditor";
import { UpdateModal } from "./common/UpdateModal";
import { OnboardingFlow } from "./onboarding/OnboardingFlow";
import { GuidedTour } from "./onboarding/tour/GuidedTour";
import { shouldShowGuidedTour } from "./onboarding/tour/tourSteps";
import { ExpirationPopup } from "./billing/ExpirationPopup";
import { AuthModal } from "./auth/AuthModal";
import { useAuth } from "@/hooks/useAuth";
import { SelectedModelMissingBanner } from "./SelectedModelMissingBanner";
import { useSettings } from "@/hooks/useSettings";
import { useOnboardingCheck } from "@/hooks/useOnboardingCheck";
import {
  useTranscriptionHistory,
  type Transcription,
} from "@/hooks/useTranscriptionHistory";
import { useNotes, type NoteMeta } from "@/hooks/useNotes";
import { useFolders } from "@/hooks/useFolders";
import { useAppLogs } from "@/hooks/useAppLogs";
import { useUpdaterContext } from "@/contexts/UpdaterContext";
import { useRecordingWorkflow } from "@/hooks/useRecordingWorkflow";
import { useNotesWorkflow } from "@/hooks/useNotesWorkflow";
import { useIsCompactLayout } from "@/hooks/useIsCompactLayout";
import { useDetachedNotesBridge } from "@/hooks/useDetachedNotesBridge";
import { useVoiceEdit } from "@/hooks/useVoiceEdit";

export default function Dashboard() {
  const { t } = useTranslation();
  const [selectedTranscription, setSelectedTranscription] =
    useState<Transcription | null>(null);
  const [isSidebarOpen, setIsSidebarOpen] = useState(false);
  const [activeTab, setActiveTab] = useState<DashboardTabId>("accueil");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [activeSettingsSection, setActiveSettingsSection] =
    useState<SettingsSectionId>("section-dictee");
  const [logsLevelFilter, setLogsLevelFilter] =
    useState<LevelFilter>(ALL_LEVELS_ON);
  const [logsSourceFilter, setLogsSourceFilter] = useState<string | null>(null);
  const [tabScrollEl, setTabScrollEl] = useState<HTMLDivElement | null>(null);

  const { settings, isLoaded: settingsLoaded, updateSetting } = useSettings();
  const { user, status: authStatus, isAuthModalOpen } = useAuth();
  const { showOnboarding, recheck: recheckOnboarding } = useOnboardingCheck(
    settings,
    settingsLoaded,
    user,
    authStatus !== "loading",
  );
  const { updateAvailable, updateInfo, showUpdateModal, setShowUpdateModal } =
    useUpdaterContext();
  const {
    transcriptions,
    addTranscription,
    deleteTranscription,
    clearHistory,
    togglePin,
  } = useTranscriptionHistory(settings.history_keep_last);
  const {
    notes,
    isLoading: notesLoading,
    createNote,
    readNote,
    updateNote,
    applySavedNoteMeta,
    deleteNote,
    searchNotes,
    toggleFavorite,
    toggleLocalOnly,
    moveNoteToFolder,
    moveNoteToFolderAtIndex,
    reorderNotesInFolder,
    reloadNotes,
    applyExternalNoteMeta,
  } = useNotes();
  const {
    folders,
    createFolder,
    renameFolder,
    deleteFolder,
    reorderFolders,
  } = useFolders();
  const { logs, clearLogs } = useAppLogs();
  const isCompact = useIsCompactLayout(sidebarCollapsed);

  const {
    isRecording,
    isTranscribing,
    handleToggleRecording,
    liveTranscript,
  } = useRecordingWorkflow({
    settings,
    addTranscription,
    onTranscriptionAdded: setSelectedTranscription,
    latestHistoryText: transcriptions[0]?.text,
  });

  const {
    openNoteIds,
    activeNoteId,
    setActiveNoteId,
    handleCreateNote,
    handleOpenNote,
    handleCloseNoteTab,
    handleDeleteNote,
    detachedNoteIds,
    handleDetachNote,
    handleNoteWindowClosed,
    handleReattachNote,
  } = useNotesWorkflow({
    createNote,
    deleteNote,
    notes,
    notesLoaded: !notesLoading,
  });

  const handleOpenNoteFromSidebar = useCallback(
    (note: NoteMeta) => {
      if (detachedNoteIds.includes(note.id)) {
        // Already detached — focus its window instead of opening a tab.
        void invoke("open_note_window", { noteId: note.id }).catch((e) =>
          console.error("Failed to focus note window:", e),
        );
        return;
      }
      handleOpenNote(note);
      setActiveTab("notes");
    },
    [detachedNoteIds, handleOpenNote],
  );

  const handleCreateNoteFromSidebar = useCallback(
    async (folderId: string | null = null) => {
      await handleCreateNote(folderId);
      setActiveTab("notes");
    },
    [handleCreateNote],
  );

  // Recreate a note from a broken note-link: create + seed with the linked
  // title. Does NOT open a tab — the editor flushes the source note first
  // then calls `handleOpenNote` via the `onOpenNoteInTab` prop.
  const handleRecreateLinkedNote = useCallback(
    async (title: string): Promise<string> => {
      const meta = await createNote(null);
      const safeTitle = title
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;");
      await updateNote(meta.id, `<h1>${safeTitle}</h1><p></p>`, title || meta.title);
      return meta.id;
    },
    [createNote, updateNote],
  );

  const handleOpenNoteInTabById = useCallback(
    (id: string) => {
      const note = notes.find((n) => n.id === id);
      if (note) {
        handleOpenNote(note);
      } else {
        // Fallback for freshly-created notes that haven't propagated to the
        // `notes` array yet — synthesize a minimal meta so the tab opens
        // immediately; the list will refresh on the next tick.
        handleOpenNote({
          id,
          title: "",
          createdAt: "",
          updatedAt: "",
          favorite: false,
          order: 0,
        });
      }
    },
    [notes, handleOpenNote],
  );

  const handleReattachRequestFromDetached = useCallback(
    async (id: string) => {
      handleReattachNote(id);
      setActiveTab("notes");
      try {
        await invoke("show_main_window");
      } catch (e) {
        console.error("Failed to show main window:", e);
      }
      try {
        await invoke("close_note_window", { noteId: id });
      } catch (e) {
        console.error("Failed to close note window:", e);
      }
    },
    [handleReattachNote],
  );

  const handleOpenRequestFromDetached = useCallback(
    async (id: string) => {
      if (detachedNoteIds.includes(id)) {
        try {
          await invoke("open_note_window", { noteId: id });
        } catch (e) {
          console.error("Failed to focus note window:", e);
        }
        return;
      }
      // The target may be brand new (recreated broken link) — refresh first.
      await reloadNotes();
      handleOpenNoteInTabById(id);
      setActiveTab("notes");
      try {
        await invoke("show_main_window");
      } catch (e) {
        console.error("Failed to show main window:", e);
      }
    },
    [detachedNoteIds, reloadNotes, handleOpenNoteInTabById],
  );

  const handleDetachedUpdated = useCallback(
    (payload: { id: string; title: string; updatedAt: string }) => {
      applyExternalNoteMeta(payload.id, payload.title, payload.updatedAt);
      // The ownership service already committed the durable sync intention.
    },
    [applyExternalNoteMeta],
  );

  const handleToggleLocalOnlyFromDetached = useCallback(
    async (id: string) => {
      try {
        await toggleLocalOnly(id);
        const data = await readNote(id);
        const session = await invoke<{profile_id:string;session_generation:number}>("get_profile_session");
        await emit("note-meta-updated", { meta: data.meta,...session });
      } catch (e) {
        console.error("Failed to toggle local-only from detached window:", e);
      }
    },
    [toggleLocalOnly, readNote],
  );

  useDetachedNotesBridge({
    onWindowClosed: handleNoteWindowClosed,
    onReattachRequest: (id) => void handleReattachRequestFromDetached(id),
    onOpenRequest: (id) => void handleOpenRequestFromDetached(id),
    onDeleteRequest: (id) => void handleDeleteNote(id),
    onDetachedUpdated: handleDetachedUpdated,
    onToggleLocalOnlyRequest: (id) => void handleToggleLocalOnlyFromDetached(id),
  });

  // Drives the Voice Edit overlay: that window has no auth/cloud context of
  // its own, so the AI calls run here.
  useVoiceEdit();

  // Open a note (created/updated from a transcription) in the Notes view.
  const handleOpenNoteFromHistory = useCallback(
    (noteId: string) => {
      handleOpenNoteInTabById(noteId);
      setActiveTab("notes");
    },
    [handleOpenNoteInTabById],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      if (selectedTranscription?.id === id) {
        setSelectedTranscription(null);
      }
      await deleteTranscription(id);
    },
    [selectedTranscription?.id, deleteTranscription],
  );

  const handleClearAll = async () => {
    setSelectedTranscription(null);
    await clearHistory();
  };

  const handleSelectTranscription = useCallback((transcription: Transcription) => {
    setSelectedTranscription(transcription);
    setIsSidebarOpen(true);
  }, []);

  const handleCloseDetails = useCallback(() => {
    setIsSidebarOpen(false);
  }, []);

  // Keep `selectedTranscription` in sync with the history: default to the
  // most recent entry, drop the selection if its row was deleted, and clear
  // it when the history becomes empty.
  useEffect(() => {
    if (!transcriptions.length) {
      if (selectedTranscription !== null) {
        setSelectedTranscription(null);
      }
      return;
    }

    if (!selectedTranscription) {
      setSelectedTranscription(transcriptions[0]);
      return;
    }

    const fresh = transcriptions.find(
      (item) => item.id === selectedTranscription.id,
    );

    if (!fresh) {
      setSelectedTranscription(transcriptions[0]);
      return;
    }

    // Keep the detail panel in sync with mutations applied to the row in the
    // list (pin toggle, future field updates). Identity equality is enough
    // because hooks return new objects whenever a transcription mutates.
    if (fresh !== selectedTranscription) {
      setSelectedTranscription(fresh);
    }
  }, [transcriptions, selectedTranscription]);

  useEffect(() => {
    if (activeTab === "logs" && !settings.developer_mode) {
      setActiveTab("accueil");
    }
  }, [activeTab, settings.developer_mode]);

  // ExpirationPopup → "Renew" button dispatches `lexena:open-settings` so we
  // can route to the Cloud section without coupling the popup to Dashboard
  // state. The Cloud section hosts the SubscribeButton.
  useEffect(() => {
    const onOpenSettings = (e: Event) => {
      const detail = (e as CustomEvent<{ section?: string }>).detail;
      setActiveTab("parametres");
      if (detail?.section) {
        // Callers may emit legacy ids of pages that were merged away; resolve
        // to the hosting page and scroll to the relevant card.
        const { section, anchor } = resolveSettingsTarget(detail.section);
        setActiveSettingsSection(section);
        if (anchor) scrollToSettingsAnchor(anchor);
      }
    };
    window.addEventListener("lexena:open-settings", onOpenSettings as EventListener);
    return () => {
      window.removeEventListener("lexena:open-settings", onOpenSettings as EventListener);
    };
  }, []);

  // Replay tour (from settings) → land on the Accueil tab so the tour, which
  // only renders there, can show. `tour_pending` is flipped by the caller.
  useEffect(() => {
    const onStartTour = () => setActiveTab("accueil");
    window.addEventListener("lexena:start-tour", onStartTour);
    return () => window.removeEventListener("lexena:start-tour", onStartTour);
  }, []);

  return (
    <div className="h-screen flex overflow-hidden bg-background">
      <DashboardSidebar
        activeTab={activeTab}
        onTabChange={setActiveTab}
        collapsed={sidebarCollapsed}
        onToggleCollapsed={() => setSidebarCollapsed(!sidebarCollapsed)}
        notes={notes}
        folders={folders}
        activeNoteId={activeNoteId}
        detachedNoteIds={detachedNoteIds}
        onOpenNote={handleOpenNoteFromSidebar}
        onCreateNote={handleCreateNoteFromSidebar}
        onToggleFavorite={toggleFavorite}
        onToggleLocalOnly={toggleLocalOnly}
        onDeleteNote={handleDeleteNote}
        searchNotes={searchNotes}
        onCreateFolder={createFolder}
        onRenameFolder={async (id, name, icon) => { await renameFolder(id, name, icon); }}
        onDeleteFolder={deleteFolder}
        onReorderFolders={reorderFolders}
        onReorderNotes={reorderNotesInFolder}
        onMoveNote={moveNoteToFolder}
        onMoveNoteToIndex={moveNoteToFolderAtIndex}
        activeSettingsSection={activeSettingsSection}
        onSettingsSectionChange={setActiveSettingsSection}
        onOpenAccountPage={() => {
          setActiveTab("parametres");
          setActiveSettingsSection("section-compte");
        }}
        transcriptions={transcriptions}
        logs={logs}
        levelFilter={logsLevelFilter}
        onLevelFilterChange={setLogsLevelFilter}
        sourceFilter={logsSourceFilter}
        onSourceFilterChange={setLogsSourceFilter}
        developerMode={settings.developer_mode}
        updateAvailable={updateAvailable}
        updateVersion={updateInfo?.version ?? null}
        onOpenUpdateModal={() => setShowUpdateModal(true)}
      />

      {/* Main area */}
      <div className="flex-1 flex flex-col overflow-hidden min-w-0">
        <DashboardHeader
          isRecording={isRecording}
          isTranscribing={isTranscribing}
          onToggleRecording={handleToggleRecording}
          activeTab={activeTab}
          sidebarCollapsed={sidebarCollapsed}
        />

        <SelectedModelMissingBanner
          onGoToSettings={() => {
            setActiveTab("parametres");
            setActiveSettingsSection("section-dictee");
            scrollToSettingsAnchor("section-transcription");
          }}
        />

        <main className="flex-1 overflow-hidden">
          {activeTab === "notes" && openNoteIds.length > 0 ? (
            <NotesEditor
              notes={notes}
              // Preserve open-order (FIFO) for the tab strip — new tabs land
              // on the right, as the workflow intends. `notes.filter(...)`
              // would instead use the `notes` array order (most-recently-
              // updated first), which made new tabs jump to the left.
              openNotes={openNoteIds
                .map((id) => notes.find((n) => n.id === id))
                .filter((n): n is NoteMeta => n !== undefined)}
              activeNoteId={activeNoteId}
              folders={folders}
              onActivateNote={setActiveNoteId}
              onOpenNoteInTab={handleOpenNoteInTabById}
              onCloseNote={handleCloseNoteTab}
              onDeleteNote={handleDeleteNote}
              onNoteSaved={applySavedNoteMeta}
              onToggleLocalOnly={toggleLocalOnly}
              onCreateNote={() => handleCreateNoteFromSidebar(null)}
              onRecreateLinkedNote={handleRecreateLinkedNote}
              onMoveNote={moveNoteToFolder}
              onCreateFolder={createFolder}
              readNote={readNote}
              onDetachNote={(id) => void handleDetachNote(id)}
              onDetachNoteAtCursor={(id) => void handleDetachNote(id, true)}
            />
          ) : activeTab === "notes" ? (
            <div className="flex items-center justify-center h-full text-muted-foreground text-sm">
              {t('dashboard.emptyNotes')}
            </div>
          ) : activeTab === "logs" ? (
            <LogsTab
              logs={logs}
              onClearLogs={clearLogs}
              levelFilter={logsLevelFilter}
              sourceFilter={logsSourceFilter}
              onSourceFilterChange={setLogsSourceFilter}
            />
          ) : (
            <div ref={setTabScrollEl} className="overflow-y-auto h-full">
              {activeTab === "parametres" ? (
                <SettingTabs
                  activeSection={activeSettingsSection}
                  onSectionChange={setActiveSettingsSection}
                />
              ) : (
                <div className="container mx-auto px-6 py-8">
                  {activeTab === "accueil" && (
                    <AccueilTab
                      notes={notes}
                      readNote={readNote}
                      historyCount={transcriptions.length}
                      isRecording={isRecording}
                      isTranscribing={isTranscribing}
                      liveTranscript={liveTranscript}
                      onToggleRecording={handleToggleRecording}
                      onOpenNote={handleOpenNoteFromSidebar}
                      onCreateNote={() => handleCreateNoteFromSidebar(null)}
                      onViewHistory={() => setActiveTab("historique")}
                      onViewNotes={() => setActiveTab("notes")}
                      onViewStatistics={() => setActiveTab("statistiques")}
                      onOpenAccountPage={() => {
                        setActiveTab("parametres");
                        setActiveSettingsSection("section-compte");
                      }}
                      onOpenAboutPage={() => {
                        setActiveTab("parametres");
                        setActiveSettingsSection("section-a-propos");
                      }}
                    />
                  )}
                  {activeTab === "historique" && (
                    <HistoriqueTab
                      transcriptions={transcriptions}
                      selectedTranscription={selectedTranscription}
                      isSidebarOpen={isSidebarOpen}
                      isCompact={isCompact}
                      scrollParent={tabScrollEl}
                      onSelectTranscription={handleSelectTranscription}
                      onCloseDetails={handleCloseDetails}
                      onDelete={handleDelete}
                      onClearAll={handleClearAll}
                      onTogglePin={togglePin}
                      onOpenNote={handleOpenNoteFromHistory}
                      onNotesMutated={reloadNotes}
                    />
                  )}
                  {activeTab === "statistiques" && (
                    <StatistiquesTab transcriptions={transcriptions} />
                  )}
                </div>
              )}
            </div>
          )}
        </main>
      </div>

      <UpdateModal
        open={showUpdateModal}
        onOpenChange={setShowUpdateModal}
        onViewDetails={() => {
          setActiveTab("parametres");
          setActiveSettingsSection("section-a-propos");
          scrollToSettingsAnchor("section-mises-a-jour");
        }}
      />

      {showOnboarding && <OnboardingFlow onComplete={recheckOnboarding} />}

      {!isAuthModalOpen &&
        !showOnboarding &&
        shouldShowGuidedTour(
          settingsLoaded,
          settings.tour_pending,
          activeTab,
        ) && (
          <GuidedTour
            onFinish={() => void updateSetting("tour_pending", false)}
          />
        )}

      <ExpirationPopup />

      <AuthModal />
    </div>
  );
}
