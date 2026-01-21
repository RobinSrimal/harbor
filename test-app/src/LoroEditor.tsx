import { useEffect, useRef } from "react";
import { EditorView, basicSetup } from "codemirror";
import { EditorState } from "@codemirror/state";
import { LoroDoc } from "loro-crdt";
import { LoroExtensions } from "loro-codemirror";

interface LoroEditorProps {
  doc: LoroDoc;
  containerName: string;
}

export function LoroEditor({ doc, containerName }: LoroEditorProps) {
  const editorRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);

  useEffect(() => {
    if (!editorRef.current || viewRef.current) return;

    console.log("[LoroEditor] Initializing CodeMirror editor");
    console.log("[LoroEditor] Container name:", containerName);

    // Create CodeMirror editor with Loro integration
    const initialText = doc.getText(containerName).toString();
    console.log("[LoroEditor] Initial text length:", initialText.length);

    const state = EditorState.create({
      doc: initialText,
      extensions: [
        basicSetup,
        LoroExtensions(
          doc,
          undefined, // No awareness/ephemeral state
          undefined, // No undo manager
          () => doc.getText(containerName) // Custom getter for the text container
        ),
      ],
    });

    const view = new EditorView({
      state,
      parent: editorRef.current,
    });

    console.log("[LoroEditor] ✓ CodeMirror editor created");
    viewRef.current = view;

    return () => {
      console.log("[LoroEditor] Destroying editor");
      view.destroy();
      viewRef.current = null;
    };
  }, [doc, containerName]);

  return <div ref={editorRef} className="loro-editor" />;
}
