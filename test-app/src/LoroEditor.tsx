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

    // Create CodeMirror editor with Loro integration
    const state = EditorState.create({
      doc: doc.getText(containerName).toString(),
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

    viewRef.current = view;

    return () => {
      view.destroy();
      viewRef.current = null;
    };
  }, [doc, containerName]);

  return <div ref={editorRef} className="loro-editor" />;
}
