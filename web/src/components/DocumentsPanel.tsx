import React, { useCallback, useEffect, useState } from "react";
import {
  ApiClientError,
  createDocument,
  deleteDocument,
  fetchDocument,
  fetchDocuments,
  fetchDocumentVersions,
  formatErrorInline,
  updateDocument,
} from "../api";
import type {
  ModuleDocumentDetail,
  ModuleDocumentRecord,
  ModuleDocumentVersionRecord,
} from "../types";
import { IconTrash } from "./Icons";

/**
 * Milestone C1 (H5) — Stored, versioned module documents & shared notebooks with optimistic
 * concurrency (`base_version` -> `409 DOC_VERSION_CONFLICT` conflict handling).
 */
export function DocumentsPanel({
  pluginId,
  projectId,
  onLoadParams,
}: {
  pluginId?: string;
  projectId?: string | null;
  onLoadParams?: (content: Record<string, unknown>, docId: string, version: number) => void;
}) {
  const [documents, setDocuments] = useState<ModuleDocumentRecord[]>([]);
  const [selectedDetail, setSelectedDetail] = useState<ModuleDocumentDetail | null>(null);
  const [versions, setVersions] = useState<ModuleDocumentVersionRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflictMsg, setConflictMsg] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // Create form
  const [newTitle, setNewTitle] = useState("");
  const [newPluginId, setNewPluginId] = useState(pluginId ?? "alithea/bc-jupyter-notebook");
  const [newContentText, setNewContentText] = useState('{\n  "cells": []\n}');

  // Edit form for selected document
  const [editTitle, setEditTitle] = useState("");
  const [editContentText, setEditContentText] = useState("");
  const [baseVersion, setBaseVersion] = useState<number>(1);

  const refreshList = useCallback(async () => {
    setError(null);
    try {
      const list = await fetchDocuments({
        ...(pluginId ? { plugin_id: pluginId } : {}),
        ...(projectId ? { project_id: projectId } : {}),
      });
      setDocuments(list);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setLoading(false);
    }
  }, [pluginId, projectId]);

  useEffect(() => {
    void refreshList();
  }, [refreshList]);

  const openDocument = async (docId: string, version?: number) => {
    setBusy(true);
    setError(null);
    setConflictMsg(null);
    setNotice(null);
    try {
      const [detail, history] = await Promise.all([
        fetchDocument(docId, version),
        fetchDocumentVersions(docId),
      ]);
      setSelectedDetail(detail);
      setVersions(history);
      setEditTitle(detail.document.title);
      setEditContentText(JSON.stringify(detail.version.content_json, null, 2));
      setBaseVersion(detail.version.version);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newTitle.trim() || !newPluginId.trim()) return;
    let parsed: Record<string, unknown>;
    try {
      const raw: unknown = JSON.parse(newContentText);
      if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
        setError("Document content must be a JSON object.");
        return;
      }
      parsed = raw as Record<string, unknown>;
    } catch {
      setError("Invalid JSON content.");
      return;
    }
    setBusy(true);
    setError(null);
    setConflictMsg(null);
    try {
      const created = await createDocument({
        plugin_id: newPluginId.trim(),
        title: newTitle.trim(),
        content: parsed,
        ...(projectId ? { project_id: projectId } : {}),
      });
      setNewTitle("");
      setSelectedDetail(created);
      setEditTitle(created.document.title);
      setEditContentText(JSON.stringify(created.version.content_json, null, 2));
      setBaseVersion(created.version.version);
      setVersions([created.version]);
      setNotice(`Created "${created.document.title}" (v${created.version.version}).`);
      await refreshList();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleSave = async () => {
    if (!selectedDetail) return;
    let parsed: Record<string, unknown>;
    try {
      const raw: unknown = JSON.parse(editContentText);
      if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
        setError("Document content must be a JSON object.");
        return;
      }
      parsed = raw as Record<string, unknown>;
    } catch {
      setError("Invalid JSON content.");
      return;
    }
    setBusy(true);
    setError(null);
    setConflictMsg(null);
    setNotice(null);
    try {
      const updated = await updateDocument(selectedDetail.document.id, {
        base_version: baseVersion,
        content: parsed,
        title: editTitle.trim() || selectedDetail.document.title,
      });
      setSelectedDetail(updated);
      setBaseVersion(updated.version.version);
      setVersions(await fetchDocumentVersions(updated.document.id));
      setNotice(`Saved version v${updated.version.version} (${updated.version.content_sha256.slice(0, 12)}).`);
      await refreshList();
    } catch (err: unknown) {
      if (err instanceof ApiClientError && (err.status === 409 || err.code === "DOC_VERSION_CONFLICT")) {
        setConflictMsg(
          `Version conflict (DOC_VERSION_CONFLICT): another person or agent updated this document beyond v${baseVersion}. Reload latest before saving.`,
        );
      } else {
        setError(formatErrorInline(err));
      }
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (docId: string) => {
    setBusy(true);
    setError(null);
    try {
      await deleteDocument(docId);
      if (selectedDetail?.document.id === docId) {
        setSelectedDetail(null);
        setVersions([]);
      }
      await refreshList();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="bc-settings-stack" data-testid="documents-panel">
      {error && (
        <div role="alert" className="bc-inline-alert bc-inline-alert-error">
          {error}
        </div>
      )}
      {conflictMsg && (
        <div
          role="alert"
          data-testid="document-version-conflict-banner"
          className="bc-inline-alert bc-inline-alert-error"
          style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 10 }}
        >
          <span>{conflictMsg}</span>
          {selectedDetail && (
            <button
              type="button"
              className="bc-btn bc-btn-secondary bc-btn-sm"
              data-testid="document-reload-latest-btn"
              onClick={() => void openDocument(selectedDetail.document.id)}
            >
              Reload Latest
            </button>
          )}
        </div>
      )}
      {notice && (
        <div role="status" className="bc-inline-alert">
          {notice}
        </div>
      )}

      <section className="bc-card">
        <div className="bc-card-header">
          <span className="bc-card-title">Stored Documents &amp; Versioned Notebooks</span>
          <button type="button" className="bc-btn bc-btn-ghost bc-btn-sm" onClick={() => void refreshList()}>
            Refresh
          </button>
        </div>
        {loading ? (
          <p className="bc-empty-note">Loading documents…</p>
        ) : documents.length === 0 ? (
          <p className="bc-empty-note">No stored documents yet. Create a versioned notebook or module document below.</p>
        ) : (
          <div className="table-scroll">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Title</th>
                  <th>Module</th>
                  <th>Latest</th>
                  <th>Updated</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {documents.map((doc) => (
                  <tr key={doc.id} data-testid={`document-row-${doc.id}`}>
                    <td>
                      <strong>{doc.title}</strong>
                      <div className="bc-caption mono">{doc.id}</div>
                    </td>
                    <td className="mono">{doc.plugin_id}</td>
                    <td className="mono">v{doc.latest_version}</td>
                    <td className="bc-caption mono">{new Date(doc.updated_at).toLocaleString()}</td>
                    <td>
                      <div style={{ display: "flex", gap: 6 }}>
                        <button
                          type="button"
                          className="bc-btn bc-btn-secondary bc-btn-sm"
                          onClick={() => void openDocument(doc.id)}
                        >
                          Open
                        </button>
                        <button
                          type="button"
                          className="bc-btn bc-btn-ghost bc-btn-sm"
                          disabled={busy}
                          onClick={() => void handleDelete(doc.id)}
                        >
                          <IconTrash size={13} />
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {selectedDetail && (
        <section className="bc-card" data-testid="document-editor-card">
          <div className="bc-card-header">
            <div>
              <span className="bc-card-title">
                {selectedDetail.document.title} · <span className="mono">v{baseVersion}</span>
              </span>
              <div className="bc-caption mono">
                SHA-256: {selectedDetail.version.content_sha256.slice(0, 16)}… · Author:{" "}
                {selectedDetail.version.author_credential_id ?? selectedDetail.version.author_user_id ?? selectedDetail.document.created_by}
              </div>
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              {onLoadParams && (
                <button
                  type="button"
                  className="bc-btn bc-btn-secondary bc-btn-sm"
                  onClick={() =>
                    onLoadParams(
                      selectedDetail.version.content_json,
                      selectedDetail.document.id,
                      selectedDetail.version.version,
                    )
                  }
                >
                  Load into Module
                </button>
              )}
              <button
                type="button"
                className="bc-btn bc-btn-primary bc-btn-sm"
                data-testid="document-save-btn"
                disabled={busy}
                onClick={() => void handleSave()}
              >
                {busy ? "Saving…" : `Save as v${baseVersion + 1}`}
              </button>
            </div>
          </div>

          <div style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 8 }}>
            <label className="bc-field">
              <span className="bc-field-label">Title</span>
              <input
                className="bc-input"
                value={editTitle}
                onChange={(e) => setEditTitle(e.target.value)}
              />
            </label>
            <label className="bc-field">
              <span className="bc-field-label">Document JSON Content (bound to workload hash on prepare)</span>
              <textarea
                className="bc-input mono"
                data-testid="document-content-textarea"
                rows={8}
                value={editContentText}
                onChange={(e) => setEditContentText(e.target.value)}
              />
            </label>
          </div>

          {versions.length > 0 && (
            <div style={{ marginTop: 12 }}>
              <div className="bc-caption" style={{ marginBottom: 6 }}>
                Version History ({versions.length})
              </div>
              <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                {versions.map((ver) => (
                  <button
                    key={ver.version}
                    type="button"
                    className={`bc-btn bc-btn-sm ${ver.version === baseVersion ? "bc-btn-primary" : "bc-btn-ghost"}`}
                    onClick={() => void openDocument(selectedDetail.document.id, ver.version)}
                  >
                    v{ver.version} ({ver.content_sha256.slice(0, 8)})
                  </button>
                ))}
              </div>
            </div>
          )}
        </section>
      )}

      <section className="bc-card">
        <div className="bc-card-title" style={{ marginBottom: 8 }}>Create Stored Document</div>
        <form className="bc-form-grid" onSubmit={(e) => void handleCreate(e)}>
          <label className="bc-field">
            <span className="bc-field-label">Title</span>
            <input
              className="bc-input"
              data-testid="new-document-title-input"
              placeholder="e.g. HLA Cohort Analysis Notebook"
              required
              value={newTitle}
              onChange={(e) => setNewTitle(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Target Module ID</span>
            <input
              className="bc-input mono"
              required
              value={newPluginId}
              onChange={(e) => setNewPluginId(e.target.value)}
            />
          </label>
          <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
            <span className="bc-field-label">Initial Content (JSON)</span>
            <textarea
              className="bc-input mono"
              rows={4}
              value={newContentText}
              onChange={(e) => setNewContentText(e.target.value)}
            />
          </label>
          <div>
            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="create-document-btn"
              disabled={busy || !newTitle.trim()}
            >
              Create Document
            </button>
          </div>
        </form>
      </section>
    </div>
  );
}
