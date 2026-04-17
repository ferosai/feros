"use client";

import { HugeiconsIcon } from "@hugeicons/react";
import {
  HardDriveUploadIcon,
  FileCodeCornerIcon,
  FileQuestionMarkIcon,
} from "@hugeicons/core-free-icons";
import { useEffect, useMemo, useRef, useState } from "react";

import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";

type ParseStatus = "empty" | "parsing" | "invalid" | "valid";
export type ImportConfigParseStatus = ParseStatus;
const MAX_IMPORT_FILE_BYTES = 5 * 1024 * 1024;

interface ImportConfigInputProps {
  rawValue: string;
  mode: "file" | "manual";
  onRawValueChange: (value: string) => void;
  onParsedChange: (config: unknown, error: string | null, status: ImportConfigParseStatus) => void;
}

export function ImportConfigInput({
  rawValue,
  mode,
  onRawValueChange,
  onParsedChange,
}: ImportConfigInputProps) {
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [fileError, setFileError] = useState<string | null>(null);
  const [selectedFileName, setSelectedFileName] = useState<string | null>(null);

  const parsed = useMemo(() => {
    const text = rawValue.trim();
    if (!text) {
      return {
        status: "empty" as ParseStatus,
        error: null,
        config: null as unknown,
      };
    }

    try {
      const parsed = JSON.parse(text) as unknown;
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return {
          status: "invalid" as ParseStatus,
          error: "Config must be a JSON object.",
          config: null as unknown,
        };
      }
      return {
        status: "valid" as ParseStatus,
        error: null,
        config: parsed,
      };
    } catch (err) {
      const message = err instanceof Error ? err.message : "Invalid JSON";
      return {
        status: "invalid" as ParseStatus,
        error: message,
        config: null as unknown,
      };
    }
  }, [rawValue]);

  const parseStatus = fileError ? "invalid" : parsed.status;

  useEffect(() => {
    onParsedChange(fileError ? null : parsed.config, fileError ?? parsed.error, parseStatus);
  }, [fileError, parseStatus, parsed, onParsedChange]);

  const handleFilePicked = async (file: File | null) => {
    if (!file) return;
    setSelectedFileName(file.name);
    if (!file.name.toLowerCase().endsWith(".json")) {
      setFileError("Only .json files are supported.");
      return;
    }
    if (file.size > MAX_IMPORT_FILE_BYTES) {
      setFileError("File is too large (max 5 MB).");
      return;
    }

    setFileError(null);
    const content = await file.text();
    onRawValueChange(content);
  };

  return (
    <div className="space-y-4">
      {mode === "file" ? (
        <div
          role="button"
          tabIndex={0}
          onClick={() => fileInputRef.current?.click()}
          onDragOver={(e) => {
            e.preventDefault();
            setIsDragging(true);
          }}
          onDragLeave={(e) => {
            e.preventDefault();
            setIsDragging(false);
          }}
          onDrop={(e) => {
            e.preventDefault();
            setIsDragging(false);
            const file = e.dataTransfer.files?.[0] ?? null;
            void handleFilePicked(file);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              fileInputRef.current?.click();
            }
          }}
          className={cn(
            "rounded-xl border border-dashed p-8 text-center transition-colors",
            isDragging ? "border-primary bg-primary/5" : "border-border hover:border-primary/60"
          )}
        >
          <input
            ref={fileInputRef}
            type="file"
            accept=".json,application/json"
            className="hidden"
            onChange={(e) => {
              const file = e.target.files?.[0] ?? null;
              void handleFilePicked(file);
              e.currentTarget.value = "";
            }}
          />
          <div className="mx-auto mb-3 flex size-12 items-center justify-center rounded-full bg-primary/10 text-primary">
            <HugeiconsIcon
              icon={
                parseStatus === "valid"
                  ? FileCodeCornerIcon
                  : parseStatus === "invalid"
                    ? FileQuestionMarkIcon
                    : HardDriveUploadIcon
              }
              className="size-5"
            />
          </div>
          {selectedFileName ? (
            <p className="truncate text-sm font-mono text-muted-foreground">{selectedFileName}</p>
          ) : (
            <>
              <p className="text-sm font-medium">Drop a JSON file here or click to browse</p>
              <p className="mt-1 text-xs text-muted-foreground">Accepted format: `.json`</p>
            </>
          )}
        </div>
      ) : (
        <Textarea
          value={rawValue}
          onChange={(e) => {
            setFileError(null);
            onRawValueChange(e.target.value);
          }}
          placeholder="Paste raw agent JSON config here"
          className="field-sizing-fixed min-h-[280px] max-h-[66vh] overflow-y-auto font-mono text-xs"
        />
      )}
    </div>
  );
}
