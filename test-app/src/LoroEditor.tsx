import { useEffect, useRef } from "react";
import { logger } from "./logger";
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

    logger.log("[LoroEditor] Initializing CodeMirror editor");
    logger.log("[LoroEditor] Container name:", containerName);

    // Get the text container - must be created BEFORE editor initialization
    // so that LoroExtensions can properly subscribe to it
    const textContainer = doc.getText(containerName);
    const initialText = textContainer.toString();
    logger.log("[LoroEditor] Initial text length:", initialText.length);
    logger.log("[LoroEditor] Initial doc version:", doc.oplogVersion());

    const state = EditorState.create({
      doc: initialText,
      extensions: [
        basicSetup,
        LoroExtensions(
          doc,
          undefined, // No awareness/ephemeral state
          undefined, // No undo manager
          () => doc.getText(containerName) // Getter returns the text container
        ),
      ],
    });

    const view = new EditorView({
      state,
      parent: editorRef.current,
    });

    logger.log("[LoroEditor] ✓ CodeMirror editor created");
    viewRef.current = view;

    // Add a subscription to log when the document changes
    const unsubscribe = doc.subscribe((event) => {
      logger.log("[LoroEditor] Document changed:", event);
      logger.log("[LoroEditor] New doc version:", doc.oplogVersion());
      logger.log("[LoroEditor] Text content:", doc.getText(containerName).toString());
    });

    return () => {
      logger.log("[LoroEditor] Destroying editor");
      unsubscribe();
      view.destroy();
      viewRef.current = null;
    };
  }, [doc, containerName]);

  return <div ref={editorRef} className="loro-editor" />;
}
