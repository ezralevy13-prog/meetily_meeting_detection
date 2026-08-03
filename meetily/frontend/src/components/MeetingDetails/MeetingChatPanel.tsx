'use client';

import React, { useState, useRef, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { MessageSquare, Send, X, Loader2, Sparkles } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { useConfig } from '@/contexts/ConfigContext';
import Analytics from '@/lib/analytics';

interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
}

/** A transcript line from the live recording, as held in TranscriptContext. */
export interface LiveTranscriptLine {
  text: string;
  audio_start_time?: number;
}

interface MeetingChatPanelProps {
  /** Saved meeting to chat about; transcript is read from the database. */
  meetingId?: string;
  /**
   * Transcript of the meeting currently being recorded. When provided, the
   * panel chats about the in-progress meeting instead of a saved one -- it
   * is read fresh on each question, so answers always cover everything said
   * up to that moment.
   */
  liveTranscript?: LiveTranscriptLine[];
  meetingTitle?: string;
}

/**
 * Ask questions about a meeting, answered by the configured summary model
 * grounded in that meeting's transcript. Fully local when the summary model
 * is Ollama or Built-in AI. Works both for saved meetings (`meetingId`) and
 * the meeting currently being recorded (`liveTranscript`).
 */
export function MeetingChatPanel({
  meetingId,
  liveTranscript,
  meetingTitle,
}: MeetingChatPanelProps) {
  const isLive = liveTranscript !== undefined;
  // Nothing transcribed yet -- the backend would just reject the question.
  const hasLiveContent = !isLive || (liveTranscript?.some((t) => t.text.trim()) ?? false);
  // Read the transcript at question time, not render time, so a long-running
  // panel doesn't answer from a stale snapshot.
  const liveTranscriptRef = useRef<LiveTranscriptLine[] | undefined>(liveTranscript);
  useEffect(() => {
    liveTranscriptRef.current = liveTranscript;
  }, [liveTranscript]);
  const { modelConfig } = useConfig();
  const [isOpen, setIsOpen] = useState(false);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [isThinking, setIsThinking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // Chat history is per-meeting
  useEffect(() => {
    setMessages([]);
    setError(null);
    setInput('');
  }, [meetingId]);

  // Keep the latest message in view
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, isThinking]);

  // Focus input when the panel opens
  useEffect(() => {
    if (isOpen) {
      inputRef.current?.focus();
    }
  }, [isOpen]);

  const sendQuestion = useCallback(async () => {
    const question = input.trim();
    if (!question || isThinking) return;

    const nextMessages: ChatMessage[] = [...messages, { role: 'user', content: question }];
    setMessages(nextMessages);
    setInput('');
    setError(null);
    setIsThinking(true);
    Analytics.trackButtonClick('meeting_chat_question', 'meeting_details');

    try {
      const answer = isLive
        ? await invoke<string>('api_chat_with_live_transcript', {
            title: meetingTitle ?? null,
            transcript: (liveTranscriptRef.current ?? []).map((t) => ({
              text: t.text,
              audio_start_time: t.audio_start_time ?? null,
            })),
            provider: modelConfig.provider,
            model: modelConfig.model,
            messages: nextMessages,
          })
        : await invoke<string>('api_chat_with_meeting', {
            meetingId,
            provider: modelConfig.provider,
            model: modelConfig.model,
            messages: nextMessages,
          });
      setMessages((prev) => [...prev, { role: 'assistant', content: answer }]);
    } catch (err) {
      const message = typeof err === 'string' ? err : (err as Error)?.message || String(err);
      console.error('Meeting chat failed:', err);
      setError(message);
      // Put the question back so the user can retry without retyping
      setMessages((prev) => prev.slice(0, -1));
      setInput(question);
    } finally {
      setIsThinking(false);
    }
  }, [input, isThinking, messages, meetingId, isLive, meetingTitle, modelConfig.provider, modelConfig.model]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendQuestion();
    }
  };

  if (!isOpen) {
    return (
      <button
        onClick={() => {
          setIsOpen(true);
          Analytics.trackButtonClick('meeting_chat_open', 'meeting_details');
        }}
        className="fixed bottom-6 right-6 z-40 flex items-center gap-2 px-4 py-3 bg-blue-600 text-white rounded-full shadow-lg hover:bg-blue-700 transition-colors"
        title={isLive ? 'Ask about what has been said so far' : 'Ask questions about this meeting'}
      >
        <MessageSquare className="w-5 h-5" />
        <span className="text-sm font-medium">{isLive ? 'Catch me up' : 'Ask'}</span>
      </button>
    );
  }

  return (
    <div className="fixed bottom-6 right-6 z-40 w-96 max-w-[calc(100vw-3rem)] h-[32rem] max-h-[calc(100vh-6rem)] bg-white rounded-xl shadow-2xl border border-gray-200 flex flex-col">
      {/* Header */}
      <div className="flex items-center justify-between px-4 py-3 border-b bg-gray-50 rounded-t-xl">
        <div className="flex items-center gap-2 min-w-0">
          <Sparkles className="w-4 h-4 text-blue-600 shrink-0" />
          <div className="min-w-0">
            <p className="text-sm font-semibold text-gray-900 truncate">
              {isLive ? 'Ask about this meeting (live)' : 'Ask about this meeting'}
            </p>
            <p className="text-xs text-gray-500 truncate">
              {modelConfig.provider === 'ollama' || modelConfig.provider === 'builtin-ai'
                ? `Local model: ${modelConfig.model || 'default'}`
                : `${modelConfig.provider}: ${modelConfig.model}`}
            </p>
          </div>
        </div>
        <button
          onClick={() => setIsOpen(false)}
          className="text-gray-400 hover:text-gray-600 shrink-0"
          title="Close"
        >
          <X className="w-5 h-5" />
        </button>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-4 py-3 space-y-3">
        {messages.length === 0 && !error && (
          <div className="text-center text-sm text-gray-500 mt-8 space-y-2">
            <MessageSquare className="w-8 h-8 mx-auto text-gray-300" />
            {isLive ? (
              <>
                <p>
                  {hasLiveContent
                    ? 'Ask about anything said so far in this meeting.'
                    : 'Waiting for the first few lines of transcript...'}
                </p>
                <p className="text-xs text-gray-400">
                  e.g. &quot;What did I just miss?&quot; &middot; &quot;Recap the last 5
                  minutes&quot; &middot; &quot;Has the deadline come up?&quot;
                </p>
              </>
            ) : (
              <>
                <p>Ask anything about {meetingTitle ? `"${meetingTitle}"` : 'this meeting'}.</p>
                <p className="text-xs text-gray-400">
                  e.g. &quot;What did we decide?&quot; &middot; &quot;What are my action
                  items?&quot; &middot; &quot;When did we discuss the budget?&quot;
                </p>
              </>
            )}
          </div>
        )}

        {messages.map((message, index) => (
          <div
            key={index}
            className={`flex ${message.role === 'user' ? 'justify-end' : 'justify-start'}`}
          >
            <div
              className={`max-w-[85%] px-3 py-2 rounded-lg text-sm whitespace-pre-wrap break-words ${
                message.role === 'user'
                  ? 'bg-blue-600 text-white rounded-br-sm'
                  : 'bg-gray-100 text-gray-900 rounded-bl-sm'
              }`}
            >
              {message.content}
            </div>
          </div>
        ))}

        {isThinking && (
          <div className="flex justify-start">
            <div className="flex items-center gap-2 px-3 py-2 bg-gray-100 rounded-lg rounded-bl-sm text-sm text-gray-500">
              <Loader2 className="w-4 h-4 animate-spin" />
              Reading the transcript...
            </div>
          </div>
        )}

        {error && (
          <div className="bg-red-50 border border-red-200 rounded-lg px-3 py-2">
            <p className="text-xs text-red-700 break-words">{error}</p>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input */}
      <div className="border-t px-3 py-3 flex items-center gap-2">
        <input
          ref={inputRef}
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder={hasLiveContent ? 'Ask a question...' : 'Waiting for transcript...'}
          disabled={isThinking || !hasLiveContent}
          className="flex-1 px-3 py-2 text-sm border border-gray-300 rounded-lg focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 disabled:bg-gray-50"
        />
        <Button
          size="sm"
          onClick={sendQuestion}
          disabled={isThinking || !input.trim() || !hasLiveContent}
          className="bg-blue-600 hover:bg-blue-700 shrink-0"
          title="Send"
        >
          <Send className="w-4 h-4" />
        </Button>
      </div>
    </div>
  );
}

export default MeetingChatPanel;
