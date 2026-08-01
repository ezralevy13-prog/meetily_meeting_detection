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

interface MeetingChatPanelProps {
  meetingId: string;
  meetingTitle?: string;
}

/**
 * Ask questions about a meeting, answered by the configured summary model
 * grounded in the meeting's stored transcript. Fully local when the summary
 * model is Ollama or Built-in AI.
 */
export function MeetingChatPanel({ meetingId, meetingTitle }: MeetingChatPanelProps) {
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
      const answer = await invoke<string>('api_chat_with_meeting', {
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
  }, [input, isThinking, messages, meetingId, modelConfig.provider, modelConfig.model]);

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
        title="Ask questions about this meeting"
      >
        <MessageSquare className="w-5 h-5" />
        <span className="text-sm font-medium">Ask</span>
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
            <p className="text-sm font-semibold text-gray-900 truncate">Ask about this meeting</p>
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
            <p>Ask anything about {meetingTitle ? `"${meetingTitle}"` : 'this meeting'}.</p>
            <p className="text-xs text-gray-400">
              e.g. &quot;What did we decide?&quot; &middot; &quot;What are my action items?&quot;
              &middot; &quot;When did we discuss the budget?&quot;
            </p>
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
          placeholder="Ask a question..."
          disabled={isThinking}
          className="flex-1 px-3 py-2 text-sm border border-gray-300 rounded-lg focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 disabled:bg-gray-50"
        />
        <Button
          size="sm"
          onClick={sendQuestion}
          disabled={isThinking || !input.trim()}
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
