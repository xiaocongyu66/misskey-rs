--
-- PostgreSQL database dump
--


-- Dumped from database version 16.15 (Debian 16.15-1.pgdg13+2)
-- Dumped by pg_dump version 16.15 (Ubuntu 16.15-1.pgdg24.04+2)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: antenna_src_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.antenna_src_enum AS ENUM (
    'home',
    'all',
    'users',
    'list',
    'users_blacklist'
);


--
-- Name: instance_suspensionstate_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.instance_suspensionstate_enum AS ENUM (
    'none',
    'manuallySuspended',
    'goneSuspended',
    'autoSuspendedForNotResponding'
);


--
-- Name: log_level_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.log_level_enum AS ENUM (
    'error',
    'warning',
    'info',
    'success',
    'debug'
);


--
-- Name: meta_sensitivemediadetection_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.meta_sensitivemediadetection_enum AS ENUM (
    'none',
    'all',
    'local',
    'remote'
);


--
-- Name: meta_sensitivemediadetectionsensitivity_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.meta_sensitivemediadetectionsensitivity_enum AS ENUM (
    'medium',
    'low',
    'high',
    'veryLow',
    'veryHigh'
);


--
-- Name: muted_note_reason_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.muted_note_reason_enum AS ENUM (
    'word',
    'manual',
    'spam',
    'other'
);


--
-- Name: note_draft_visibility_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.note_draft_visibility_enum AS ENUM (
    'public',
    'home',
    'followers',
    'specified'
);


--
-- Name: note_visibility_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.note_visibility_enum AS ENUM (
    'public',
    'home',
    'followers',
    'specified'
);


--
-- Name: notification_type_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.notification_type_enum AS ENUM (
    'follow',
    'mention',
    'reply',
    'renote',
    'quote',
    'reaction',
    'pollVote',
    'pollEnded',
    'receiveFollowRequest',
    'followRequestAccepted',
    'groupInvited',
    'achievementEarned',
    'app'
);


--
-- Name: page_visibility_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.page_visibility_enum AS ENUM (
    'public',
    'followers',
    'specified'
);


--
-- Name: poll_notevisibility_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.poll_notevisibility_enum AS ENUM (
    'public',
    'home',
    'followers',
    'specified'
);


--
-- Name: relay_status_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.relay_status_enum AS ENUM (
    'requesting',
    'accepted',
    'rejected'
);


--
-- Name: role_target_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.role_target_enum AS ENUM (
    'manual',
    'conditional'
);


--
-- Name: user_profile_followersvisibility_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.user_profile_followersvisibility_enum AS ENUM (
    'public',
    'followers',
    'private'
);


--
-- Name: user_profile_followingvisibility_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.user_profile_followingvisibility_enum AS ENUM (
    'public',
    'followers',
    'private'
);


--
-- Name: user_profile_mutingnotificationtypes_enum; Type: TYPE; Schema: public; Owner: -
--

CREATE TYPE public.user_profile_mutingnotificationtypes_enum AS ENUM (
    'note',
    'follow',
    'mention',
    'reply',
    'renote',
    'quote',
    'reaction',
    'pollEnded',
    'receiveFollowRequest',
    'followRequestAccepted',
    'achievementEarned',
    'app',
    'test',
    'pollVote',
    'groupInvited'
);


--
-- Name: get_birthday_date(text); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.get_birthday_date(birthday text) RETURNS smallint
    LANGUAGE plpgsql IMMUTABLE
    AS $$ BEGIN RETURN CAST((SUBSTR(birthday, 6, 2) || SUBSTR(birthday, 9, 2)) AS SMALLINT); END; $$;


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: __chart__active_users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__active_users (
    id integer NOT NULL,
    date integer NOT NULL,
    "unique_temp___registeredWithinWeek" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredWithinWeek" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredWithinMonth" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredWithinMonth" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredWithinYear" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredWithinYear" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredOutsideWeek" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredOutsideWeek" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredOutsideMonth" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredOutsideMonth" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredOutsideYear" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredOutsideYear" integer DEFAULT '0'::smallint NOT NULL,
    "___readWrite" integer DEFAULT '0'::smallint NOT NULL,
    unique_temp___read character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___read integer DEFAULT '0'::smallint NOT NULL,
    unique_temp___write character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___write integer DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart__active_users_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__active_users_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__active_users_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__active_users_id_seq OWNED BY public.__chart__active_users.id;


--
-- Name: __chart__ap_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__ap_request (
    id integer NOT NULL,
    date integer NOT NULL,
    "___deliverFailed" integer DEFAULT 0 NOT NULL,
    "___deliverSucceeded" integer DEFAULT 0 NOT NULL,
    "___inboxReceived" integer DEFAULT 0 NOT NULL
);


--
-- Name: __chart__ap_request_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__ap_request_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__ap_request_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__ap_request_id_seq OWNED BY public.__chart__ap_request.id;


--
-- Name: __chart__drive; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__drive (
    id integer NOT NULL,
    date integer NOT NULL,
    "___local_incCount" integer DEFAULT '0'::bigint NOT NULL,
    "___local_incSize" integer DEFAULT '0'::bigint NOT NULL,
    "___local_decCount" integer DEFAULT '0'::bigint NOT NULL,
    "___local_decSize" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_incCount" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_incSize" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_decCount" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_decSize" integer DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart__drive_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__drive_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__drive_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__drive_id_seq OWNED BY public.__chart__drive.id;


--
-- Name: __chart__federation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__federation (
    id integer NOT NULL,
    date integer NOT NULL,
    "unique_temp___deliveredInstances" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___deliveredInstances" smallint DEFAULT '0'::smallint NOT NULL,
    "unique_temp___inboxInstances" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___inboxInstances" smallint DEFAULT '0'::smallint NOT NULL,
    unique_temp___stalled character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___stalled smallint DEFAULT '0'::smallint NOT NULL,
    ___sub smallint DEFAULT '0'::smallint NOT NULL,
    ___pub smallint DEFAULT '0'::smallint NOT NULL,
    ___pubsub smallint DEFAULT '0'::smallint NOT NULL,
    "___subActive" smallint DEFAULT '0'::smallint NOT NULL,
    "___pubActive" smallint DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart__federation_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__federation_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__federation_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__federation_id_seq OWNED BY public.__chart__federation.id;


--
-- Name: __chart__hashtag; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__hashtag (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___local_users integer DEFAULT 0 NOT NULL,
    ___remote_users integer DEFAULT 0 NOT NULL,
    unique_temp___local_users character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    unique_temp___remote_users character varying[] DEFAULT '{}'::character varying[] NOT NULL
);


--
-- Name: __chart__hashtag_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__hashtag_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__hashtag_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__hashtag_id_seq OWNED BY public.__chart__hashtag.id;


--
-- Name: __chart__instance; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__instance (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___requests_failed smallint DEFAULT '0'::bigint NOT NULL,
    ___requests_succeeded smallint DEFAULT '0'::bigint NOT NULL,
    ___requests_received smallint DEFAULT '0'::bigint NOT NULL,
    ___notes_total integer DEFAULT '0'::bigint NOT NULL,
    ___notes_inc integer DEFAULT '0'::bigint NOT NULL,
    ___notes_dec integer DEFAULT '0'::bigint NOT NULL,
    ___notes_diffs_normal integer DEFAULT '0'::bigint NOT NULL,
    ___notes_diffs_reply integer DEFAULT '0'::bigint NOT NULL,
    ___notes_diffs_renote integer DEFAULT '0'::bigint NOT NULL,
    ___users_total integer DEFAULT '0'::bigint NOT NULL,
    ___users_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___users_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___following_total integer DEFAULT '0'::bigint NOT NULL,
    ___following_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___following_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___followers_total integer DEFAULT '0'::bigint NOT NULL,
    ___followers_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___followers_dec smallint DEFAULT '0'::bigint NOT NULL,
    "___drive_totalFiles" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_incFiles" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_incUsage" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_decFiles" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_decUsage" integer DEFAULT '0'::bigint NOT NULL,
    "___notes_diffs_withFile" integer DEFAULT 0 NOT NULL
);


--
-- Name: __chart__instance_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__instance_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__instance_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__instance_id_seq OWNED BY public.__chart__instance.id;


--
-- Name: __chart__network; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__network (
    id integer NOT NULL,
    date integer NOT NULL,
    "___incomingRequests" integer DEFAULT '0'::bigint NOT NULL,
    "___outgoingRequests" integer DEFAULT '0'::bigint NOT NULL,
    "___totalTime" integer DEFAULT '0'::bigint NOT NULL,
    "___incomingBytes" integer DEFAULT '0'::bigint NOT NULL,
    "___outgoingBytes" integer DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart__network_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__network_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__network_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__network_id_seq OWNED BY public.__chart__network.id;


--
-- Name: __chart__notes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__notes (
    id integer NOT NULL,
    date integer NOT NULL,
    ___local_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_inc integer DEFAULT '0'::bigint NOT NULL,
    ___local_dec integer DEFAULT '0'::bigint NOT NULL,
    ___local_diffs_normal integer DEFAULT '0'::bigint NOT NULL,
    ___local_diffs_reply integer DEFAULT '0'::bigint NOT NULL,
    ___local_diffs_renote integer DEFAULT '0'::bigint NOT NULL,
    ___remote_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_inc integer DEFAULT '0'::bigint NOT NULL,
    ___remote_dec integer DEFAULT '0'::bigint NOT NULL,
    ___remote_diffs_normal integer DEFAULT '0'::bigint NOT NULL,
    ___remote_diffs_reply integer DEFAULT '0'::bigint NOT NULL,
    ___remote_diffs_renote integer DEFAULT '0'::bigint NOT NULL,
    "___local_diffs_withFile" integer DEFAULT 0 NOT NULL,
    "___remote_diffs_withFile" integer DEFAULT 0 NOT NULL
);


--
-- Name: __chart__notes_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__notes_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__notes_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__notes_id_seq OWNED BY public.__chart__notes.id;


--
-- Name: __chart__per_user_drive; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__per_user_drive (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    "___totalCount" integer DEFAULT '0'::bigint NOT NULL,
    "___totalSize" integer DEFAULT '0'::bigint NOT NULL,
    "___incCount" smallint DEFAULT '0'::bigint NOT NULL,
    "___incSize" integer DEFAULT '0'::bigint NOT NULL,
    "___decCount" smallint DEFAULT '0'::bigint NOT NULL,
    "___decSize" integer DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart__per_user_drive_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__per_user_drive_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__per_user_drive_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__per_user_drive_id_seq OWNED BY public.__chart__per_user_drive.id;


--
-- Name: __chart__per_user_following; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__per_user_following (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___local_followings_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_followings_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___local_followings_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___local_followers_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_followers_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___local_followers_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followings_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_followings_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followings_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followers_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_followers_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followers_dec smallint DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart__per_user_following_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__per_user_following_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__per_user_following_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__per_user_following_id_seq OWNED BY public.__chart__per_user_following.id;


--
-- Name: __chart__per_user_notes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__per_user_notes (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___total integer DEFAULT '0'::bigint NOT NULL,
    ___inc smallint DEFAULT '0'::bigint NOT NULL,
    ___dec smallint DEFAULT '0'::bigint NOT NULL,
    ___diffs_normal smallint DEFAULT '0'::bigint NOT NULL,
    ___diffs_reply smallint DEFAULT '0'::bigint NOT NULL,
    ___diffs_renote smallint DEFAULT '0'::bigint NOT NULL,
    "___diffs_withFile" smallint DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart__per_user_notes_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__per_user_notes_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__per_user_notes_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__per_user_notes_id_seq OWNED BY public.__chart__per_user_notes.id;


--
-- Name: __chart__per_user_pv; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__per_user_pv (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    unique_temp___upv_user character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___upv_user smallint DEFAULT '0'::smallint NOT NULL,
    ___pv_user smallint DEFAULT '0'::smallint NOT NULL,
    unique_temp___upv_visitor character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___upv_visitor smallint DEFAULT '0'::smallint NOT NULL,
    ___pv_visitor smallint DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart__per_user_pv_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__per_user_pv_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__per_user_pv_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__per_user_pv_id_seq OWNED BY public.__chart__per_user_pv.id;


--
-- Name: __chart__per_user_reaction; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__per_user_reaction (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___local_count smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_count smallint DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart__per_user_reaction_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__per_user_reaction_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__per_user_reaction_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__per_user_reaction_id_seq OWNED BY public.__chart__per_user_reaction.id;


--
-- Name: __chart__test; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__test (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128),
    ___foo_total bigint NOT NULL,
    ___foo_inc bigint NOT NULL,
    ___foo_dec bigint NOT NULL
);


--
-- Name: __chart__test_grouped; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__test_grouped (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128),
    ___foo_total bigint NOT NULL,
    ___foo_inc bigint NOT NULL,
    ___foo_dec bigint NOT NULL
);


--
-- Name: __chart__test_grouped_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__test_grouped_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__test_grouped_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__test_grouped_id_seq OWNED BY public.__chart__test_grouped.id;


--
-- Name: __chart__test_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__test_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__test_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__test_id_seq OWNED BY public.__chart__test.id;


--
-- Name: __chart__test_unique; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__test_unique (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128),
    ___foo character varying[] DEFAULT '{}'::character varying[] NOT NULL
);


--
-- Name: __chart__test_unique_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__test_unique_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__test_unique_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__test_unique_id_seq OWNED BY public.__chart__test_unique.id;


--
-- Name: __chart__users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart__users (
    id integer NOT NULL,
    date integer NOT NULL,
    ___local_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___local_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_dec smallint DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart__users_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart__users_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart__users_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart__users_id_seq OWNED BY public.__chart__users.id;


--
-- Name: __chart_day__active_users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__active_users (
    id integer NOT NULL,
    date integer NOT NULL,
    "unique_temp___registeredWithinWeek" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredWithinWeek" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredWithinMonth" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredWithinMonth" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredWithinYear" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredWithinYear" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredOutsideWeek" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredOutsideWeek" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredOutsideMonth" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredOutsideMonth" integer DEFAULT '0'::smallint NOT NULL,
    "unique_temp___registeredOutsideYear" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___registeredOutsideYear" integer DEFAULT '0'::smallint NOT NULL,
    "___readWrite" integer DEFAULT '0'::smallint NOT NULL,
    unique_temp___read character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___read integer DEFAULT '0'::smallint NOT NULL,
    unique_temp___write character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___write integer DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart_day__active_users_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__active_users_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__active_users_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__active_users_id_seq OWNED BY public.__chart_day__active_users.id;


--
-- Name: __chart_day__ap_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__ap_request (
    id integer NOT NULL,
    date integer NOT NULL,
    "___deliverFailed" integer DEFAULT 0 NOT NULL,
    "___deliverSucceeded" integer DEFAULT 0 NOT NULL,
    "___inboxReceived" integer DEFAULT 0 NOT NULL
);


--
-- Name: __chart_day__ap_request_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__ap_request_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__ap_request_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__ap_request_id_seq OWNED BY public.__chart_day__ap_request.id;


--
-- Name: __chart_day__drive; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__drive (
    id integer NOT NULL,
    date integer NOT NULL,
    "___local_incCount" integer DEFAULT '0'::bigint NOT NULL,
    "___local_incSize" integer DEFAULT '0'::bigint NOT NULL,
    "___local_decCount" integer DEFAULT '0'::bigint NOT NULL,
    "___local_decSize" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_incCount" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_incSize" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_decCount" integer DEFAULT '0'::bigint NOT NULL,
    "___remote_decSize" integer DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart_day__drive_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__drive_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__drive_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__drive_id_seq OWNED BY public.__chart_day__drive.id;


--
-- Name: __chart_day__federation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__federation (
    id integer NOT NULL,
    date integer NOT NULL,
    "unique_temp___deliveredInstances" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___deliveredInstances" smallint DEFAULT '0'::smallint NOT NULL,
    "unique_temp___inboxInstances" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "___inboxInstances" smallint DEFAULT '0'::smallint NOT NULL,
    unique_temp___stalled character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___stalled smallint DEFAULT '0'::smallint NOT NULL,
    ___sub smallint DEFAULT '0'::smallint NOT NULL,
    ___pub smallint DEFAULT '0'::smallint NOT NULL,
    ___pubsub smallint DEFAULT '0'::smallint NOT NULL,
    "___subActive" smallint DEFAULT '0'::smallint NOT NULL,
    "___pubActive" smallint DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart_day__federation_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__federation_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__federation_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__federation_id_seq OWNED BY public.__chart_day__federation.id;


--
-- Name: __chart_day__hashtag; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__hashtag (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___local_users integer DEFAULT 0 NOT NULL,
    ___remote_users integer DEFAULT 0 NOT NULL,
    unique_temp___local_users character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    unique_temp___remote_users character varying[] DEFAULT '{}'::character varying[] NOT NULL
);


--
-- Name: __chart_day__hashtag_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__hashtag_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__hashtag_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__hashtag_id_seq OWNED BY public.__chart_day__hashtag.id;


--
-- Name: __chart_day__instance; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__instance (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___requests_failed smallint DEFAULT '0'::bigint NOT NULL,
    ___requests_succeeded smallint DEFAULT '0'::bigint NOT NULL,
    ___requests_received smallint DEFAULT '0'::bigint NOT NULL,
    ___notes_total integer DEFAULT '0'::bigint NOT NULL,
    ___notes_inc integer DEFAULT '0'::bigint NOT NULL,
    ___notes_dec integer DEFAULT '0'::bigint NOT NULL,
    ___notes_diffs_normal integer DEFAULT '0'::bigint NOT NULL,
    ___notes_diffs_reply integer DEFAULT '0'::bigint NOT NULL,
    ___notes_diffs_renote integer DEFAULT '0'::bigint NOT NULL,
    ___users_total integer DEFAULT '0'::bigint NOT NULL,
    ___users_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___users_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___following_total integer DEFAULT '0'::bigint NOT NULL,
    ___following_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___following_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___followers_total integer DEFAULT '0'::bigint NOT NULL,
    ___followers_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___followers_dec smallint DEFAULT '0'::bigint NOT NULL,
    "___drive_totalFiles" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_incFiles" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_incUsage" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_decFiles" integer DEFAULT '0'::bigint NOT NULL,
    "___drive_decUsage" integer DEFAULT '0'::bigint NOT NULL,
    "___notes_diffs_withFile" integer DEFAULT 0 NOT NULL
);


--
-- Name: __chart_day__instance_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__instance_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__instance_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__instance_id_seq OWNED BY public.__chart_day__instance.id;


--
-- Name: __chart_day__network; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__network (
    id integer NOT NULL,
    date integer NOT NULL,
    "___incomingRequests" integer DEFAULT '0'::bigint NOT NULL,
    "___outgoingRequests" integer DEFAULT '0'::bigint NOT NULL,
    "___totalTime" integer DEFAULT '0'::bigint NOT NULL,
    "___incomingBytes" integer DEFAULT '0'::bigint NOT NULL,
    "___outgoingBytes" integer DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart_day__network_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__network_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__network_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__network_id_seq OWNED BY public.__chart_day__network.id;


--
-- Name: __chart_day__notes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__notes (
    id integer NOT NULL,
    date integer NOT NULL,
    ___local_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_inc integer DEFAULT '0'::bigint NOT NULL,
    ___local_dec integer DEFAULT '0'::bigint NOT NULL,
    ___local_diffs_normal integer DEFAULT '0'::bigint NOT NULL,
    ___local_diffs_reply integer DEFAULT '0'::bigint NOT NULL,
    ___local_diffs_renote integer DEFAULT '0'::bigint NOT NULL,
    ___remote_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_inc integer DEFAULT '0'::bigint NOT NULL,
    ___remote_dec integer DEFAULT '0'::bigint NOT NULL,
    ___remote_diffs_normal integer DEFAULT '0'::bigint NOT NULL,
    ___remote_diffs_reply integer DEFAULT '0'::bigint NOT NULL,
    ___remote_diffs_renote integer DEFAULT '0'::bigint NOT NULL,
    "___local_diffs_withFile" integer DEFAULT 0 NOT NULL,
    "___remote_diffs_withFile" integer DEFAULT 0 NOT NULL
);


--
-- Name: __chart_day__notes_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__notes_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__notes_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__notes_id_seq OWNED BY public.__chart_day__notes.id;


--
-- Name: __chart_day__per_user_drive; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__per_user_drive (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    "___totalCount" integer DEFAULT '0'::bigint NOT NULL,
    "___totalSize" integer DEFAULT '0'::bigint NOT NULL,
    "___incCount" smallint DEFAULT '0'::bigint NOT NULL,
    "___incSize" integer DEFAULT '0'::bigint NOT NULL,
    "___decCount" smallint DEFAULT '0'::bigint NOT NULL,
    "___decSize" integer DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart_day__per_user_drive_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__per_user_drive_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__per_user_drive_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__per_user_drive_id_seq OWNED BY public.__chart_day__per_user_drive.id;


--
-- Name: __chart_day__per_user_following; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__per_user_following (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___local_followings_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_followings_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___local_followings_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___local_followers_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_followers_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___local_followers_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followings_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_followings_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followings_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followers_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_followers_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_followers_dec smallint DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart_day__per_user_following_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__per_user_following_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__per_user_following_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__per_user_following_id_seq OWNED BY public.__chart_day__per_user_following.id;


--
-- Name: __chart_day__per_user_notes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__per_user_notes (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___total integer DEFAULT '0'::bigint NOT NULL,
    ___inc smallint DEFAULT '0'::bigint NOT NULL,
    ___dec smallint DEFAULT '0'::bigint NOT NULL,
    ___diffs_normal smallint DEFAULT '0'::bigint NOT NULL,
    ___diffs_reply smallint DEFAULT '0'::bigint NOT NULL,
    ___diffs_renote smallint DEFAULT '0'::bigint NOT NULL,
    "___diffs_withFile" smallint DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart_day__per_user_notes_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__per_user_notes_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__per_user_notes_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__per_user_notes_id_seq OWNED BY public.__chart_day__per_user_notes.id;


--
-- Name: __chart_day__per_user_pv; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__per_user_pv (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    unique_temp___upv_user character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___upv_user smallint DEFAULT '0'::smallint NOT NULL,
    ___pv_user smallint DEFAULT '0'::smallint NOT NULL,
    unique_temp___upv_visitor character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    ___upv_visitor smallint DEFAULT '0'::smallint NOT NULL,
    ___pv_visitor smallint DEFAULT '0'::smallint NOT NULL
);


--
-- Name: __chart_day__per_user_pv_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__per_user_pv_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__per_user_pv_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__per_user_pv_id_seq OWNED BY public.__chart_day__per_user_pv.id;


--
-- Name: __chart_day__per_user_reaction; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__per_user_reaction (
    id integer NOT NULL,
    date integer NOT NULL,
    "group" character varying(128) NOT NULL,
    ___local_count smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_count smallint DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart_day__per_user_reaction_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__per_user_reaction_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__per_user_reaction_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__per_user_reaction_id_seq OWNED BY public.__chart_day__per_user_reaction.id;


--
-- Name: __chart_day__users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.__chart_day__users (
    id integer NOT NULL,
    date integer NOT NULL,
    ___local_total integer DEFAULT '0'::bigint NOT NULL,
    ___local_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___local_dec smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_total integer DEFAULT '0'::bigint NOT NULL,
    ___remote_inc smallint DEFAULT '0'::bigint NOT NULL,
    ___remote_dec smallint DEFAULT '0'::bigint NOT NULL
);


--
-- Name: __chart_day__users_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.__chart_day__users_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: __chart_day__users_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.__chart_day__users_id_seq OWNED BY public.__chart_day__users.id;


--
-- Name: abuse_report_notification_recipient; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.abuse_report_notification_recipient (
    id character varying(32) NOT NULL,
    "isActive" boolean DEFAULT true NOT NULL,
    "updatedAt" timestamp with time zone DEFAULT now() NOT NULL,
    name character varying(255) NOT NULL,
    method character varying(64) NOT NULL,
    "userId" character varying(32),
    "systemWebhookId" character varying(32)
);


--
-- Name: abuse_user_report; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.abuse_user_report (
    id character varying(32) NOT NULL,
    "targetUserId" character varying(32) NOT NULL,
    "reporterId" character varying(32) NOT NULL,
    "assigneeId" character varying(32),
    resolved boolean DEFAULT false NOT NULL,
    comment character varying(2048) NOT NULL,
    "targetUserHost" character varying(128),
    "reporterHost" character varying(128),
    forwarded boolean DEFAULT false NOT NULL,
    "moderationNote" character varying(8192) DEFAULT ''::character varying NOT NULL,
    "resolvedAs" character varying(128)
);


--
-- Name: COLUMN abuse_user_report."targetUserHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.abuse_user_report."targetUserHost" IS '[Denormalized]';


--
-- Name: COLUMN abuse_user_report."reporterHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.abuse_user_report."reporterHost" IS '[Denormalized]';


--
-- Name: access_token; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.access_token (
    id character varying(32) NOT NULL,
    token character varying(128) NOT NULL,
    hash character varying(128) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "appId" character varying(32),
    "lastUsedAt" timestamp with time zone,
    session character varying(128),
    name character varying(128),
    description character varying(512),
    "iconUrl" character varying(512),
    permission character varying(64)[] DEFAULT '{}'::character varying[] NOT NULL,
    fetched boolean DEFAULT false NOT NULL
);


--
-- Name: ad; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ad (
    id character varying(32) NOT NULL,
    "expiresAt" timestamp with time zone NOT NULL,
    place character varying(32) NOT NULL,
    priority character varying(32) NOT NULL,
    url character varying(1024) NOT NULL,
    "imageUrl" character varying(1024) NOT NULL,
    memo character varying(8192) NOT NULL,
    ratio integer DEFAULT 1 NOT NULL,
    "startsAt" timestamp with time zone DEFAULT now() NOT NULL,
    "dayOfWeek" integer DEFAULT 0 NOT NULL,
    "isSensitive" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN ad."expiresAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.ad."expiresAt" IS 'The expired date of the Ad.';


--
-- Name: COLUMN ad."startsAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.ad."startsAt" IS 'The expired date of the Ad.';


--
-- Name: announcement; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.announcement (
    id character varying(32) NOT NULL,
    text character varying(8192) NOT NULL,
    title character varying(256) NOT NULL,
    "imageUrl" character varying(1024),
    "updatedAt" timestamp with time zone,
    display character varying(256) DEFAULT 'normal'::character varying NOT NULL,
    "needConfirmationToRead" boolean DEFAULT false NOT NULL,
    "isActive" boolean DEFAULT true NOT NULL,
    "forExistingUsers" boolean DEFAULT false NOT NULL,
    "userId" character varying(32),
    icon character varying(256) DEFAULT 'info'::character varying NOT NULL,
    silence boolean DEFAULT false NOT NULL,
    "autoArchiveAt" timestamp with time zone
);


--
-- Name: COLUMN announcement."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.announcement."updatedAt" IS 'The updated date of the Announcement.';


--
-- Name: COLUMN announcement."autoArchiveAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.announcement."autoArchiveAt" IS 'The date after which the Announcement is automatically archived.';


--
-- Name: announcement_read; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.announcement_read (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "announcementId" character varying(32) NOT NULL
);


--
-- Name: antenna; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.antenna (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    name character varying(128) NOT NULL,
    src public.antenna_src_enum NOT NULL,
    "userListId" character varying(32),
    keywords jsonb DEFAULT '[]'::jsonb NOT NULL,
    "withFile" boolean NOT NULL,
    expression character varying(2048),
    "caseSensitive" boolean DEFAULT false NOT NULL,
    "withReplies" boolean DEFAULT false NOT NULL,
    users character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "excludeKeywords" jsonb DEFAULT '[]'::jsonb NOT NULL,
    "lastUsedAt" timestamp with time zone NOT NULL,
    "isActive" boolean DEFAULT true NOT NULL,
    "localOnly" boolean DEFAULT false NOT NULL,
    "excludeBots" boolean DEFAULT false NOT NULL,
    "excludeNotesInSensitiveChannel" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN antenna."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.antenna."userId" IS 'The owner ID.';


--
-- Name: COLUMN antenna.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.antenna.name IS 'The name of the Antenna.';


--
-- Name: app; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.app (
    id character varying(32) NOT NULL,
    "userId" character varying(32),
    secret character varying(64) NOT NULL,
    name character varying(128) NOT NULL,
    description character varying(512) NOT NULL,
    permission character varying(64)[] NOT NULL,
    "callbackUrl" character varying(512)
);


--
-- Name: COLUMN app."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.app."userId" IS 'The owner ID.';


--
-- Name: COLUMN app.secret; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.app.secret IS 'The secret key of the App.';


--
-- Name: COLUMN app.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.app.name IS 'The name of the App.';


--
-- Name: COLUMN app.description; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.app.description IS 'The description of the App.';


--
-- Name: COLUMN app.permission; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.app.permission IS 'The permission of the App.';


--
-- Name: COLUMN app."callbackUrl"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.app."callbackUrl" IS 'The callbackUrl of the App.';


--
-- Name: auth_session; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.auth_session (
    id character varying(32) NOT NULL,
    token character varying(128) NOT NULL,
    "userId" character varying(32),
    "appId" character varying(32) NOT NULL
);


--
-- Name: avatar_decoration; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.avatar_decoration (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone,
    url character varying(1024) NOT NULL,
    name character varying(256) NOT NULL,
    description character varying(2048) NOT NULL,
    "roleIdsThatCanBeUsedThisDecoration" character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    category character varying(128)
);


--
-- Name: blocking; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.blocking (
    id character varying(32) NOT NULL,
    "blockeeId" character varying(32) NOT NULL,
    "blockerId" character varying(32) NOT NULL
);


--
-- Name: COLUMN blocking."blockeeId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.blocking."blockeeId" IS 'The blockee user ID.';


--
-- Name: COLUMN blocking."blockerId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.blocking."blockerId" IS 'The blocker user ID.';


--
-- Name: bubble_game_record; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.bubble_game_record (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "seededAt" timestamp with time zone NOT NULL,
    seed character varying(1024) NOT NULL,
    "gameVersion" integer NOT NULL,
    "gameMode" character varying(128) NOT NULL,
    score integer NOT NULL,
    logs jsonb DEFAULT '[]'::jsonb NOT NULL,
    "isVerified" boolean DEFAULT false NOT NULL
);


--
-- Name: channel; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.channel (
    id character varying(32) NOT NULL,
    "lastNotedAt" timestamp with time zone,
    "userId" character varying(32),
    name character varying(128) NOT NULL,
    description character varying(2048),
    "bannerId" character varying(32),
    "notesCount" integer DEFAULT 0 NOT NULL,
    "usersCount" integer DEFAULT 0 NOT NULL,
    "pinnedNoteIds" character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    color character varying(16) DEFAULT '#86b300'::character varying NOT NULL,
    "isArchived" boolean DEFAULT false NOT NULL,
    "isSensitive" boolean DEFAULT false NOT NULL,
    "allowRenoteToExternal" boolean DEFAULT true NOT NULL
);


--
-- Name: COLUMN channel."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel."userId" IS 'The owner ID.';


--
-- Name: COLUMN channel.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel.name IS 'The name of the Channel.';


--
-- Name: COLUMN channel.description; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel.description IS 'The description of the Channel.';


--
-- Name: COLUMN channel."bannerId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel."bannerId" IS 'The ID of banner Channel.';


--
-- Name: COLUMN channel."notesCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel."notesCount" IS 'The count of notes.';


--
-- Name: COLUMN channel."usersCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel."usersCount" IS 'The count of users.';


--
-- Name: channel_favorite; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.channel_favorite (
    id character varying(32) NOT NULL,
    "channelId" character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL
);


--
-- Name: channel_following; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.channel_following (
    id character varying(32) NOT NULL,
    "followeeId" character varying(32) NOT NULL,
    "followerId" character varying(32) NOT NULL
);


--
-- Name: COLUMN channel_following."followeeId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel_following."followeeId" IS 'The followee channel ID.';


--
-- Name: COLUMN channel_following."followerId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel_following."followerId" IS 'The follower user ID.';


--
-- Name: channel_muting; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.channel_muting (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "channelId" character varying(32) NOT NULL,
    "expiresAt" timestamp with time zone
);


--
-- Name: channel_note_pining; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.channel_note_pining (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "channelId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL
);


--
-- Name: COLUMN channel_note_pining."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.channel_note_pining."createdAt" IS 'The created date of the ChannelNotePining.';


--
-- Name: chat_approval; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.chat_approval (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "otherId" character varying(32) NOT NULL
);


--
-- Name: chat_message; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.chat_message (
    id character varying(32) NOT NULL,
    "fromUserId" character varying(32) NOT NULL,
    "toUserId" character varying(32),
    "toRoomId" character varying(32),
    text character varying(4096),
    uri character varying(512),
    reads character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "fileId" character varying(32),
    reactions character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL
);


--
-- Name: chat_room; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.chat_room (
    id character varying(32) NOT NULL,
    name character varying(256) NOT NULL,
    "ownerId" character varying(32) NOT NULL,
    description character varying(2048) DEFAULT ''::character varying NOT NULL,
    "isArchived" boolean DEFAULT false NOT NULL
);


--
-- Name: chat_room_invitation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.chat_room_invitation (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "roomId" character varying(32) NOT NULL,
    ignored boolean DEFAULT false NOT NULL
);


--
-- Name: chat_room_membership; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.chat_room_membership (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "roomId" character varying(32) NOT NULL,
    "isMuted" boolean DEFAULT false NOT NULL
);


--
-- Name: clip; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.clip (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    name character varying(128) NOT NULL,
    "isPublic" boolean DEFAULT false NOT NULL,
    description character varying(2048),
    "lastClippedAt" timestamp with time zone
);


--
-- Name: COLUMN clip."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.clip."userId" IS 'The owner ID.';


--
-- Name: COLUMN clip.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.clip.name IS 'The name of the Clip.';


--
-- Name: COLUMN clip.description; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.clip.description IS 'The description of the Clip.';


--
-- Name: clip_favorite; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.clip_favorite (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "clipId" character varying(32) NOT NULL
);


--
-- Name: clip_note; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.clip_note (
    id character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL,
    "clipId" character varying(32) NOT NULL
);


--
-- Name: COLUMN clip_note."noteId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.clip_note."noteId" IS 'The note ID.';


--
-- Name: COLUMN clip_note."clipId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.clip_note."clipId" IS 'The clip ID.';


--
-- Name: drive_file; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.drive_file (
    id character varying(32) NOT NULL,
    "userId" character varying(32),
    "userHost" character varying(128),
    md5 character varying(32) NOT NULL,
    name character varying(256) NOT NULL,
    type character varying(128) NOT NULL,
    size integer NOT NULL,
    comment character varying(512),
    properties jsonb DEFAULT '{}'::jsonb NOT NULL,
    "storedInternal" boolean NOT NULL,
    url character varying(1024) NOT NULL,
    "thumbnailUrl" character varying(512),
    "webpublicUrl" character varying(512),
    "accessKey" character varying(256),
    "thumbnailAccessKey" character varying(256),
    "webpublicAccessKey" character varying(256),
    uri character varying(1024),
    src character varying(1024),
    "folderId" character varying(32),
    "isSensitive" boolean DEFAULT false NOT NULL,
    "isLink" boolean DEFAULT false NOT NULL,
    blurhash character varying(128),
    "webpublicType" character varying(128),
    "requestHeaders" jsonb DEFAULT '{}'::jsonb,
    "requestIp" character varying(128),
    "maybeSensitive" boolean DEFAULT false NOT NULL,
    "maybePorn" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN drive_file."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."userId" IS 'The owner ID.';


--
-- Name: COLUMN drive_file."userHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."userHost" IS 'The host of owner. It will be null if the user in local.';


--
-- Name: COLUMN drive_file.md5; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.md5 IS 'The MD5 hash of the DriveFile.';


--
-- Name: COLUMN drive_file.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.name IS 'The file name of the DriveFile.';


--
-- Name: COLUMN drive_file.type; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.type IS 'The content type (MIME) of the DriveFile.';


--
-- Name: COLUMN drive_file.size; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.size IS 'The file size (bytes) of the DriveFile.';


--
-- Name: COLUMN drive_file.comment; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.comment IS 'The comment of the DriveFile.';


--
-- Name: COLUMN drive_file.properties; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.properties IS 'The any properties of the DriveFile. For example, it includes image width/height.';


--
-- Name: COLUMN drive_file.url; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.url IS 'The URL of the DriveFile.';


--
-- Name: COLUMN drive_file."thumbnailUrl"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."thumbnailUrl" IS 'The URL of the thumbnail of the DriveFile.';


--
-- Name: COLUMN drive_file."webpublicUrl"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."webpublicUrl" IS 'The URL of the webpublic of the DriveFile.';


--
-- Name: COLUMN drive_file.uri; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.uri IS 'The URI of the DriveFile. it will be null when the DriveFile is local.';


--
-- Name: COLUMN drive_file."folderId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."folderId" IS 'The parent folder ID. If null, it means the DriveFile is located in root.';


--
-- Name: COLUMN drive_file."isSensitive"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."isSensitive" IS 'Whether the DriveFile is NSFW.';


--
-- Name: COLUMN drive_file."isLink"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."isLink" IS 'Whether the DriveFile is direct link to remote server.';


--
-- Name: COLUMN drive_file.blurhash; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file.blurhash IS 'The BlurHash string.';


--
-- Name: COLUMN drive_file."maybeSensitive"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_file."maybeSensitive" IS 'Whether the DriveFile is NSFW. (predict)';


--
-- Name: drive_folder; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.drive_folder (
    id character varying(32) NOT NULL,
    name character varying(128) NOT NULL,
    "userId" character varying(32),
    "parentId" character varying(32)
);


--
-- Name: COLUMN drive_folder.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_folder.name IS 'The name of the DriveFolder.';


--
-- Name: COLUMN drive_folder."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_folder."userId" IS 'The owner ID.';


--
-- Name: COLUMN drive_folder."parentId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.drive_folder."parentId" IS 'The parent folder ID. If null, it means the DriveFolder is located in root.';


--
-- Name: emoji; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.emoji (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone,
    name character varying(128) NOT NULL,
    host character varying(128),
    "originalUrl" character varying(512) NOT NULL,
    uri character varying(512),
    type character varying(64),
    aliases character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    category character varying(128),
    "publicUrl" character varying(512) DEFAULT ''::character varying NOT NULL,
    license character varying(1024),
    "localOnly" boolean DEFAULT false NOT NULL,
    "isSensitive" boolean DEFAULT false NOT NULL,
    "roleIdsThatCanBeUsedThisEmojiAsReaction" character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL
);


--
-- Name: flash; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.flash (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    title character varying(256) NOT NULL,
    summary character varying(1024) NOT NULL,
    "userId" character varying(32) NOT NULL,
    script character varying(65536) NOT NULL,
    permissions character varying(256)[] DEFAULT '{}'::character varying[] NOT NULL,
    "likedCount" integer DEFAULT 0 NOT NULL,
    visibility character varying(512) DEFAULT 'public'::character varying NOT NULL
);


--
-- Name: COLUMN flash."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.flash."updatedAt" IS 'The updated date of the Flash.';


--
-- Name: COLUMN flash."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.flash."userId" IS 'The ID of author.';


--
-- Name: flash_like; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.flash_like (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "flashId" character varying(32) NOT NULL
);


--
-- Name: follow_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.follow_request (
    id character varying(32) NOT NULL,
    "followeeId" character varying(32) NOT NULL,
    "followerId" character varying(32) NOT NULL,
    "requestId" character varying(128),
    "followerHost" character varying(128),
    "followerInbox" character varying(512),
    "followerSharedInbox" character varying(512),
    "followeeHost" character varying(128),
    "followeeInbox" character varying(512),
    "followeeSharedInbox" character varying(512),
    "withReplies" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN follow_request."followeeId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followeeId" IS 'The followee user ID.';


--
-- Name: COLUMN follow_request."followerId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followerId" IS 'The follower user ID.';


--
-- Name: COLUMN follow_request."requestId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."requestId" IS 'id of Follow Activity.';


--
-- Name: COLUMN follow_request."followerHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followerHost" IS '[Denormalized]';


--
-- Name: COLUMN follow_request."followerInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followerInbox" IS '[Denormalized]';


--
-- Name: COLUMN follow_request."followerSharedInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followerSharedInbox" IS '[Denormalized]';


--
-- Name: COLUMN follow_request."followeeHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followeeHost" IS '[Denormalized]';


--
-- Name: COLUMN follow_request."followeeInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followeeInbox" IS '[Denormalized]';


--
-- Name: COLUMN follow_request."followeeSharedInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.follow_request."followeeSharedInbox" IS '[Denormalized]';


--
-- Name: following; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.following (
    id character varying(32) NOT NULL,
    "followeeId" character varying(32) NOT NULL,
    "followerId" character varying(32) NOT NULL,
    "followerHost" character varying(128),
    "followerInbox" character varying(512),
    "followerSharedInbox" character varying(512),
    "followeeHost" character varying(128),
    "followeeInbox" character varying(512),
    "followeeSharedInbox" character varying(512),
    notify character varying(32),
    "withReplies" boolean DEFAULT false NOT NULL,
    "isFollowerHibernated" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN following."followeeId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followeeId" IS 'The followee user ID.';


--
-- Name: COLUMN following."followerId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followerId" IS 'The follower user ID.';


--
-- Name: COLUMN following."followerHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followerHost" IS '[Denormalized]';


--
-- Name: COLUMN following."followerInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followerInbox" IS '[Denormalized]';


--
-- Name: COLUMN following."followerSharedInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followerSharedInbox" IS '[Denormalized]';


--
-- Name: COLUMN following."followeeHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followeeHost" IS '[Denormalized]';


--
-- Name: COLUMN following."followeeInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followeeInbox" IS '[Denormalized]';


--
-- Name: COLUMN following."followeeSharedInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.following."followeeSharedInbox" IS '[Denormalized]';


--
-- Name: gallery_like; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.gallery_like (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "postId" character varying(32) NOT NULL
);


--
-- Name: gallery_post; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.gallery_post (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    title character varying(256) NOT NULL,
    description character varying(2048),
    "userId" character varying(32) NOT NULL,
    "fileIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "isSensitive" boolean DEFAULT false NOT NULL,
    "likedCount" integer DEFAULT 0 NOT NULL,
    tags character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL
);


--
-- Name: COLUMN gallery_post."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.gallery_post."updatedAt" IS 'The updated date of the GalleryPost.';


--
-- Name: COLUMN gallery_post."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.gallery_post."userId" IS 'The ID of author.';


--
-- Name: COLUMN gallery_post."isSensitive"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.gallery_post."isSensitive" IS 'Whether the post is sensitive.';


--
-- Name: hashtag; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.hashtag (
    id character varying(32) NOT NULL,
    name character varying(128) NOT NULL,
    "mentionedUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "mentionedUsersCount" integer DEFAULT 0 NOT NULL,
    "mentionedLocalUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "mentionedLocalUsersCount" integer DEFAULT 0 NOT NULL,
    "mentionedRemoteUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "mentionedRemoteUsersCount" integer DEFAULT 0 NOT NULL,
    "attachedUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "attachedUsersCount" integer DEFAULT 0 NOT NULL,
    "attachedLocalUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "attachedLocalUsersCount" integer DEFAULT 0 NOT NULL,
    "attachedRemoteUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "attachedRemoteUsersCount" integer DEFAULT 0 NOT NULL
);


--
-- Name: instance; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.instance (
    id character varying(32) NOT NULL,
    "firstRetrievedAt" timestamp with time zone NOT NULL,
    host character varying(128) NOT NULL,
    "usersCount" integer DEFAULT 0 NOT NULL,
    "notesCount" integer DEFAULT 0 NOT NULL,
    "followingCount" integer DEFAULT 0 NOT NULL,
    "followersCount" integer DEFAULT 0 NOT NULL,
    "latestRequestReceivedAt" timestamp with time zone,
    "isNotResponding" boolean DEFAULT false NOT NULL,
    "softwareName" character varying(64),
    "softwareVersion" character varying(64),
    "openRegistrations" boolean,
    name character varying(256),
    description character varying(4096),
    "maintainerName" character varying(128),
    "maintainerEmail" character varying(256),
    "infoUpdatedAt" timestamp with time zone,
    "suspensionState" public.instance_suspensionstate_enum DEFAULT 'none'::public.instance_suspensionstate_enum NOT NULL,
    "iconUrl" character varying(256),
    "themeColor" character varying(64),
    "faviconUrl" character varying(256),
    "moderationNote" character varying(16384) DEFAULT ''::character varying NOT NULL,
    "notRespondingSince" timestamp with time zone
);


--
-- Name: COLUMN instance."firstRetrievedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.instance."firstRetrievedAt" IS 'The caught date of the Instance.';


--
-- Name: COLUMN instance.host; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.instance.host IS 'The host of the Instance.';


--
-- Name: COLUMN instance."usersCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.instance."usersCount" IS 'The count of the users of the Instance.';


--
-- Name: COLUMN instance."notesCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.instance."notesCount" IS 'The count of the notes of the Instance.';


--
-- Name: COLUMN instance."softwareName"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.instance."softwareName" IS 'The software of the Instance.';


--
-- Name: messaging_message; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.messaging_message (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    "recipientId" character varying(32),
    text character varying(4096),
    "isRead" boolean DEFAULT false NOT NULL,
    "fileId" character varying(32),
    "groupId" character varying(32),
    reads character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    uri character varying(512)
);


--
-- Name: COLUMN messaging_message."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.messaging_message."createdAt" IS 'The created date of the MessagingMessage.';


--
-- Name: COLUMN messaging_message."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.messaging_message."userId" IS 'The sender user ID.';


--
-- Name: COLUMN messaging_message."recipientId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.messaging_message."recipientId" IS 'The recipient user ID.';


--
-- Name: COLUMN messaging_message."groupId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.messaging_message."groupId" IS 'The recipient group ID.';


--
-- Name: meta; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.meta (
    id character varying(32) NOT NULL,
    name character varying(1024),
    description character varying(1024),
    "maintainerName" character varying(1024),
    "maintainerEmail" character varying(1024),
    "disableRegistration" boolean DEFAULT true NOT NULL,
    langs character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "hiddenTags" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "blockedHosts" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "mascotImageUrl" character varying(1024),
    "bannerUrl" character varying(1024),
    "iconUrl" character varying(1024),
    "cacheRemoteFiles" boolean DEFAULT false NOT NULL,
    "enableRecaptcha" boolean DEFAULT false NOT NULL,
    "recaptchaSiteKey" character varying(1024),
    "recaptchaSecretKey" character varying(1024),
    "urlPreviewSummaryProxyUrl" character varying(1024),
    "enableEmail" boolean DEFAULT false NOT NULL,
    email character varying(1024),
    "smtpSecure" boolean DEFAULT false NOT NULL,
    "smtpHost" character varying(1024),
    "smtpPort" integer,
    "smtpUser" character varying(1024),
    "smtpPass" character varying(1024),
    "enableServiceWorker" boolean DEFAULT false NOT NULL,
    "swPublicKey" character varying(1024),
    "swPrivateKey" character varying(1024),
    "pinnedUsers" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "termsOfServiceUrl" character varying(1024),
    "repositoryUrl" character varying(1024) DEFAULT 'https://github.com/misskey-dev/misskey'::character varying,
    "feedbackUrl" character varying(1024) DEFAULT 'https://github.com/misskey-dev/misskey/issues/new'::character varying,
    "useObjectStorage" boolean DEFAULT false NOT NULL,
    "objectStorageBucket" character varying(1024),
    "objectStoragePrefix" character varying(1024),
    "objectStorageBaseUrl" character varying(1024),
    "objectStorageEndpoint" character varying(1024),
    "objectStorageRegion" character varying(1024),
    "objectStorageAccessKey" character varying(1024),
    "objectStorageSecretKey" character varying(1024),
    "objectStoragePort" integer,
    "objectStorageUseSSL" boolean DEFAULT true NOT NULL,
    "objectStorageUseProxy" boolean DEFAULT true NOT NULL,
    "enableHcaptcha" boolean DEFAULT false NOT NULL,
    "hcaptchaSiteKey" character varying(1024),
    "hcaptchaSecretKey" character varying(1024),
    "objectStorageSetPublicRead" boolean DEFAULT false NOT NULL,
    "backgroundImageUrl" character varying(1024),
    "logoImageUrl" character varying(1024),
    "objectStorageS3ForcePathStyle" boolean DEFAULT true NOT NULL,
    "deeplAuthKey" character varying(1024),
    "deeplIsPro" boolean DEFAULT false NOT NULL,
    "emailRequiredForSignup" boolean DEFAULT false NOT NULL,
    "themeColor" character varying(1024),
    "defaultLightTheme" character varying(8192),
    "defaultDarkTheme" character varying(8192),
    "sensitiveMediaDetection" public.meta_sensitivemediadetection_enum DEFAULT 'none'::public.meta_sensitivemediadetection_enum NOT NULL,
    "sensitiveMediaDetectionSensitivity" public.meta_sensitivemediadetectionsensitivity_enum DEFAULT 'medium'::public.meta_sensitivemediadetectionsensitivity_enum NOT NULL,
    "setSensitiveFlagAutomatically" boolean DEFAULT false NOT NULL,
    "enableIpLogging" boolean DEFAULT false NOT NULL,
    "enableSensitiveMediaDetectionForVideos" boolean DEFAULT false NOT NULL,
    "enableActiveEmailValidation" boolean DEFAULT true NOT NULL,
    "enableTurnstile" boolean DEFAULT false NOT NULL,
    "turnstileSiteKey" character varying(1024),
    "turnstileSecretKey" character varying(1024),
    policies jsonb DEFAULT '{}'::jsonb NOT NULL,
    "sensitiveWords" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "enableChartsForRemoteUser" boolean DEFAULT true NOT NULL,
    "enableChartsForFederatedInstances" boolean DEFAULT true NOT NULL,
    "serverRules" character varying(280)[] DEFAULT '{}'::character varying[] NOT NULL,
    "preservedUsernames" character varying(1024)[] DEFAULT '{admin,administrator,root,system,maintainer,host,mod,moderator,owner,superuser,staff,auth,i,me,everyone,all,mention,mentions,example,user,users,account,accounts,official,help,helps,support,supports,info,information,informations,announce,announces,announcement,announcements,notice,notification,notifications,dev,developer,developers,tech,misskey}'::character varying[] NOT NULL,
    "serverErrorImageUrl" character varying(1024),
    "notFoundImageUrl" character varying(1024),
    "infoImageUrl" character varying(1024),
    "enableServerMachineStats" boolean DEFAULT false NOT NULL,
    "enableIdenticonGeneration" boolean DEFAULT true NOT NULL,
    "cacheRemoteSensitiveFiles" boolean DEFAULT true NOT NULL,
    "app192IconUrl" character varying(1024),
    "app512IconUrl" character varying(1024),
    "manifestJsonOverride" character varying(8192) DEFAULT '{}'::character varying NOT NULL,
    "shortName" character varying(64),
    "impressumUrl" character varying(1024),
    "privacyPolicyUrl" character varying(1024),
    "perLocalUserUserTimelineCacheMax" integer DEFAULT 300 NOT NULL,
    "perRemoteUserUserTimelineCacheMax" integer DEFAULT 100 NOT NULL,
    "perUserHomeTimelineCacheMax" integer DEFAULT 300 NOT NULL,
    "perUserListTimelineCacheMax" integer DEFAULT 300 NOT NULL,
    "notesPerOneAd" integer DEFAULT 0 NOT NULL,
    "silencedHosts" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "enableFanoutTimeline" boolean DEFAULT true NOT NULL,
    "enableFanoutTimelineDbFallback" boolean DEFAULT true NOT NULL,
    "verifymailAuthKey" character varying(1024),
    "enableVerifymailApi" boolean DEFAULT false NOT NULL,
    "bannedEmailDomains" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "truemailInstance" character varying(1024),
    "truemailAuthKey" character varying(1024),
    "enableTruemailApi" boolean DEFAULT false NOT NULL,
    "enableMcaptcha" boolean DEFAULT false NOT NULL,
    "mcaptchaSitekey" character varying(1024),
    "mcaptchaSecretKey" character varying(1024),
    "mcaptchaInstanceUrl" character varying(1024),
    "prohibitedWords" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "urlPreviewEnabled" boolean DEFAULT true NOT NULL,
    "urlPreviewTimeout" integer DEFAULT 10000 NOT NULL,
    "urlPreviewMaximumContentLength" bigint DEFAULT 10485760 NOT NULL,
    "urlPreviewRequireContentLength" boolean DEFAULT false NOT NULL,
    "urlPreviewUserAgent" character varying(1024),
    "mediaSilencedHosts" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "inquiryUrl" character varying(1024),
    "enableReactionsBuffering" boolean DEFAULT false NOT NULL,
    "enableStatsForFederatedInstances" boolean DEFAULT true NOT NULL,
    federation character varying(128) DEFAULT 'none'::character varying NOT NULL,
    "federationHosts" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "enableTestcaptcha" boolean DEFAULT false NOT NULL,
    "prohibitedWordsForNameOfUser" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "googleAnalyticsMeasurementId" character varying(64),
    "rootUserId" character varying(32),
    "deliverSuspendedSoftware" jsonb DEFAULT '[]'::jsonb NOT NULL,
    "ugcVisibilityForVisitor" character varying(128) DEFAULT 'local'::character varying NOT NULL,
    "singleUserMode" boolean DEFAULT false NOT NULL,
    "proxyRemoteFiles" boolean DEFAULT true NOT NULL,
    "signToActivityPubGet" boolean DEFAULT true NOT NULL,
    "allowExternalApRedirect" boolean DEFAULT true NOT NULL,
    "urlPreviewAllowRedirect" boolean DEFAULT true NOT NULL,
    "enableRemoteNotesCleaning" boolean DEFAULT false NOT NULL,
    "remoteNotesCleaningMaxProcessingDurationInMinutes" integer DEFAULT 60 NOT NULL,
    "remoteNotesCleaningExpiryDaysForEachNotes" integer DEFAULT 90 NOT NULL,
    "clientOptions" jsonb DEFAULT '{}'::jsonb NOT NULL,
    "showRoleBadgesOfRemoteUsers" boolean DEFAULT false NOT NULL,
    "sensitiveMediaDetectionApiUrl" character varying(1024),
    "sensitiveMediaDetectionApiKey" character varying(1024),
    "sensitiveMediaDetectionTimeout" integer DEFAULT 60000 NOT NULL,
    "sensitiveMediaDetectionMaxImagesPerRequest" integer DEFAULT 4 NOT NULL,
    "urlPreviewSensitiveList" character varying(3072)[] DEFAULT '{}'::character varying[] NOT NULL
);


--
--



--
-- Name: moderation_log; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.moderation_log (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    type character varying(128) NOT NULL,
    info jsonb NOT NULL
);


--
-- Name: muting; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.muting (
    id character varying(32) NOT NULL,
    "muteeId" character varying(32) NOT NULL,
    "muterId" character varying(32) NOT NULL,
    "expiresAt" timestamp with time zone
);


--
-- Name: COLUMN muting."muteeId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.muting."muteeId" IS 'The mutee user ID.';


--
-- Name: COLUMN muting."muterId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.muting."muterId" IS 'The muter user ID.';


--
-- Name: note; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note (
    id character varying(32) NOT NULL,
    "replyId" character varying(32),
    "renoteId" character varying(32),
    text text,
    name character varying(256),
    cw character varying(512),
    "userId" character varying(32) NOT NULL,
    "localOnly" boolean DEFAULT false NOT NULL,
    "renoteCount" smallint DEFAULT 0 NOT NULL,
    "repliesCount" smallint DEFAULT 0 NOT NULL,
    reactions jsonb DEFAULT '{}'::jsonb NOT NULL,
    visibility public.note_visibility_enum NOT NULL,
    uri character varying(512),
    "fileIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "attachedFileTypes" character varying(256)[] DEFAULT '{}'::character varying[] NOT NULL,
    "visibleUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    mentions character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "mentionedRemoteUsers" text DEFAULT '[]'::text NOT NULL,
    emojis character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    tags character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    "hasPoll" boolean DEFAULT false NOT NULL,
    "userHost" character varying(128),
    "replyUserId" character varying(32),
    "replyUserHost" character varying(128),
    "renoteUserId" character varying(32),
    "renoteUserHost" character varying(128),
    url character varying(512),
    "channelId" character varying(32),
    "threadId" character varying(256),
    "reactionAcceptance" character varying(64),
    "clippedCount" smallint DEFAULT '0'::smallint NOT NULL,
    "reactionAndUserPairCache" character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    "pageCount" smallint DEFAULT '0'::smallint NOT NULL,
    "renoteChannelId" character varying(32)
);


--
-- Name: COLUMN note."replyId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."replyId" IS 'The ID of reply target.';


--
-- Name: COLUMN note."renoteId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."renoteId" IS 'The ID of renote target.';


--
-- Name: COLUMN note."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."userId" IS 'The ID of author.';


--
-- Name: COLUMN note.uri; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note.uri IS 'The URI of a note. it will be null when the note is local.';


--
-- Name: COLUMN note."userHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."userHost" IS '[Denormalized]';


--
-- Name: COLUMN note."replyUserId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."replyUserId" IS '[Denormalized]';


--
-- Name: COLUMN note."replyUserHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."replyUserHost" IS '[Denormalized]';


--
-- Name: COLUMN note."renoteUserId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."renoteUserId" IS '[Denormalized]';


--
-- Name: COLUMN note."renoteUserHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."renoteUserHost" IS '[Denormalized]';


--
-- Name: COLUMN note.url; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note.url IS 'The human readable url of a note. it will be null when the note is local.';


--
-- Name: COLUMN note."channelId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."channelId" IS 'The ID of source channel.';


--
-- Name: COLUMN note."renoteChannelId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note."renoteChannelId" IS '[Denormalized]';


--
-- Name: note_draft; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note_draft (
    id character varying(32) NOT NULL,
    "replyId" character varying(32),
    "renoteId" character varying(32),
    text text,
    cw character varying(512),
    "userId" character varying(32) NOT NULL,
    "localOnly" boolean DEFAULT false NOT NULL,
    "reactionAcceptance" character varying(64),
    visibility public.note_draft_visibility_enum NOT NULL,
    "fileIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "visibleUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    hashtag character varying(128),
    "channelId" character varying(32),
    "hasPoll" boolean DEFAULT false NOT NULL,
    "pollChoices" character varying(256)[] DEFAULT '{}'::character varying[] NOT NULL,
    "pollMultiple" boolean NOT NULL,
    "pollExpiresAt" timestamp with time zone,
    "pollExpiredAfter" bigint,
    "scheduledAt" timestamp with time zone,
    "isActuallyScheduled" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN note_draft."replyId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_draft."replyId" IS 'The ID of reply target.';


--
-- Name: COLUMN note_draft."renoteId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_draft."renoteId" IS 'The ID of renote target.';


--
-- Name: COLUMN note_draft."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_draft."userId" IS 'The ID of author.';


--
-- Name: COLUMN note_draft."channelId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_draft."channelId" IS 'The ID of source channel.';


--
-- Name: note_favorite; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note_favorite (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL
);


--
-- Name: note_reaction; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note_reaction (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL,
    reaction character varying(260) NOT NULL
);


--
-- Name: note_thread_muting; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note_thread_muting (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "threadId" character varying(256) NOT NULL
);


--
-- Name: note_unread; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note_unread (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL,
    "noteUserId" character varying(32) NOT NULL,
    "isSpecified" boolean NOT NULL,
    "isMentioned" boolean NOT NULL,
    "noteChannelId" character varying(32)
);


--
-- Name: COLUMN note_unread."noteUserId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_unread."noteUserId" IS '[Denormalized]';


--
-- Name: COLUMN note_unread."noteChannelId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_unread."noteChannelId" IS '[Denormalized]';


--
-- Name: note_watching; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.note_watching (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL,
    "noteUserId" character varying(32) NOT NULL
);


--
-- Name: COLUMN note_watching."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_watching."createdAt" IS 'The created date of the NoteWatching.';


--
-- Name: COLUMN note_watching."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_watching."userId" IS 'The watcher ID.';


--
-- Name: COLUMN note_watching."noteId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_watching."noteId" IS 'The target Note ID.';


--
-- Name: COLUMN note_watching."noteUserId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.note_watching."noteUserId" IS '[Denormalized]';


--
-- Name: page; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.page (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    title character varying(256) NOT NULL,
    name character varying(256) NOT NULL,
    summary character varying(256),
    "alignCenter" boolean NOT NULL,
    font character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "eyeCatchingImageId" character varying(32),
    content jsonb DEFAULT '[]'::jsonb NOT NULL,
    variables jsonb DEFAULT '[]'::jsonb NOT NULL,
    visibility public.page_visibility_enum NOT NULL,
    "visibleUserIds" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "likedCount" integer DEFAULT 0 NOT NULL,
    "hideTitleWhenPinned" boolean DEFAULT false NOT NULL,
    script character varying(16384) DEFAULT ''::character varying NOT NULL
);


--
-- Name: COLUMN page."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.page."updatedAt" IS 'The updated date of the Page.';


--
-- Name: COLUMN page."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.page."userId" IS 'The ID of author.';


--
-- Name: page_like; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.page_like (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "pageId" character varying(32) NOT NULL
);


--
-- Name: password_reset_request; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.password_reset_request (
    id character varying(32) NOT NULL,
    token character varying(256) NOT NULL,
    "userId" character varying(32) NOT NULL
);


--
-- Name: poll; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.poll (
    "noteId" character varying(32) NOT NULL,
    "expiresAt" timestamp with time zone,
    multiple boolean NOT NULL,
    choices character varying(256)[] DEFAULT '{}'::character varying[] NOT NULL,
    votes integer[] NOT NULL,
    "noteVisibility" public.poll_notevisibility_enum NOT NULL,
    "userId" character varying(32) NOT NULL,
    "userHost" character varying(128),
    "channelId" character varying(32)
);


--
-- Name: COLUMN poll."noteVisibility"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.poll."noteVisibility" IS '[Denormalized]';


--
-- Name: COLUMN poll."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.poll."userId" IS '[Denormalized]';


--
-- Name: COLUMN poll."userHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.poll."userHost" IS '[Denormalized]';


--
-- Name: COLUMN poll."channelId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.poll."channelId" IS '[Denormalized]';


--
-- Name: poll_vote; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.poll_vote (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL,
    choice integer NOT NULL
);


--
-- Name: promo_note; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.promo_note (
    "noteId" character varying(32) NOT NULL,
    "expiresAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL
);


--
-- Name: COLUMN promo_note."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.promo_note."userId" IS '[Denormalized]';


--
-- Name: promo_read; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.promo_read (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL
);


--
-- Name: registration_ticket; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.registration_ticket (
    id character varying(32) NOT NULL,
    code character varying(64) NOT NULL,
    "expiresAt" timestamp with time zone,
    "usedAt" timestamp with time zone,
    "pendingUserId" character varying(32),
    "createdById" character varying(32),
    "usedById" character varying(32)
);


--
-- Name: registry_item; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.registry_item (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    key character varying(1024) NOT NULL,
    scope character varying(1024)[] DEFAULT '{}'::character varying[] NOT NULL,
    domain character varying(512),
    value jsonb DEFAULT '{}'::jsonb
);


--
-- Name: COLUMN registry_item."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.registry_item."updatedAt" IS 'The updated date of the RegistryItem.';


--
-- Name: COLUMN registry_item."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.registry_item."userId" IS 'The owner ID.';


--
-- Name: COLUMN registry_item.key; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.registry_item.key IS 'The key of the RegistryItem.';


--
-- Name: COLUMN registry_item.value; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.registry_item.value IS 'The value of the RegistryItem.';


--
-- Name: relay; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.relay (
    id character varying(32) NOT NULL,
    inbox character varying(512) NOT NULL,
    status public.relay_status_enum NOT NULL
);


--
-- Name: renote_muting; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.renote_muting (
    id character varying(32) NOT NULL,
    "muteeId" character varying(32) NOT NULL,
    "muterId" character varying(32) NOT NULL
);


--
-- Name: COLUMN renote_muting."muteeId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.renote_muting."muteeId" IS 'The mutee user ID.';


--
-- Name: COLUMN renote_muting."muterId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.renote_muting."muterId" IS 'The muter user ID.';


--
-- Name: retention_aggregation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.retention_aggregation (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userIds" character varying(32)[] NOT NULL,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    "usersCount" integer NOT NULL,
    "dateKey" character varying(512) NOT NULL
);


--
-- Name: COLUMN retention_aggregation."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.retention_aggregation."createdAt" IS 'The created date of the Note.';


--
-- Name: COLUMN retention_aggregation."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.retention_aggregation."updatedAt" IS 'The updated date of the GalleryPost.';


--
-- Name: reversi_game; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.reversi_game (
    id character varying(32) NOT NULL,
    "startedAt" timestamp with time zone,
    "user1Id" character varying(32) NOT NULL,
    "user2Id" character varying(32) NOT NULL,
    "user1Ready" boolean DEFAULT false NOT NULL,
    "user2Ready" boolean DEFAULT false NOT NULL,
    black integer,
    "isStarted" boolean DEFAULT false NOT NULL,
    "isEnded" boolean DEFAULT false NOT NULL,
    "winnerId" character varying(32),
    "surrenderedUserId" character varying(32),
    logs jsonb DEFAULT '[]'::jsonb NOT NULL,
    map character varying(64)[] NOT NULL,
    bw character varying(32) NOT NULL,
    "isLlotheo" boolean DEFAULT false NOT NULL,
    "canPutEverywhere" boolean DEFAULT false NOT NULL,
    "loopedBoard" boolean DEFAULT false NOT NULL,
    form1 jsonb,
    form2 jsonb,
    crc32 character varying(32),
    "timeoutUserId" character varying(32),
    "endedAt" timestamp with time zone,
    "timeLimitForEachTurn" smallint DEFAULT '90'::smallint NOT NULL,
    "noIrregularRules" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN reversi_game."startedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.reversi_game."startedAt" IS 'The started date of the ReversiGame.';


--
-- Name: COLUMN reversi_game."endedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.reversi_game."endedAt" IS 'The ended date of the ReversiGame.';


--
-- Name: reversi_matching; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.reversi_matching (
    id character varying(32) NOT NULL,
    "parentId" character varying(32) NOT NULL,
    "childId" character varying(32) NOT NULL
);


--
-- Name: role; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.role (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone NOT NULL,
    name character varying(256) NOT NULL,
    description character varying(1024) NOT NULL,
    "isPublic" boolean DEFAULT false NOT NULL,
    "isModerator" boolean DEFAULT false NOT NULL,
    "isAdministrator" boolean DEFAULT false NOT NULL,
    policies jsonb DEFAULT '{}'::jsonb NOT NULL,
    color character varying(256),
    "canEditMembersByModerator" boolean DEFAULT false NOT NULL,
    "lastUsedAt" timestamp with time zone NOT NULL,
    target public.role_target_enum DEFAULT 'manual'::public.role_target_enum NOT NULL,
    "condFormula" jsonb DEFAULT '{}'::jsonb NOT NULL,
    "iconUrl" character varying(512),
    "asBadge" boolean DEFAULT false NOT NULL,
    "displayOrder" integer DEFAULT 0 NOT NULL,
    "isExplorable" boolean DEFAULT false NOT NULL,
    "preserveAssignmentOnMoveAccount" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN role."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.role."updatedAt" IS 'The updated date of the Role.';


--
-- Name: COLUMN role."lastUsedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.role."lastUsedAt" IS 'The last used date of the Role.';


--
-- Name: role_assignment; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.role_assignment (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "roleId" character varying(32) NOT NULL,
    "expiresAt" timestamp with time zone
);


--
-- Name: COLUMN role_assignment."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.role_assignment."userId" IS 'The user ID.';


--
-- Name: COLUMN role_assignment."roleId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.role_assignment."roleId" IS 'The role ID.';


--
-- Name: signin; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.signin (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    ip character varying(128) NOT NULL,
    headers jsonb NOT NULL,
    success boolean NOT NULL
);


--
-- Name: sw_subscription; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.sw_subscription (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    endpoint character varying(512) NOT NULL,
    auth character varying(256) NOT NULL,
    publickey character varying(128) NOT NULL,
    "sendReadMessage" boolean DEFAULT false NOT NULL
);


--
-- Name: system_account; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.system_account (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    type character varying(256) NOT NULL
);


--
-- Name: system_webhook; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.system_webhook (
    id character varying(32) NOT NULL,
    "isActive" boolean DEFAULT true NOT NULL,
    "updatedAt" timestamp with time zone DEFAULT now() NOT NULL,
    "latestSentAt" timestamp with time zone,
    "latestStatus" integer,
    name character varying(255) NOT NULL,
    "on" character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    url character varying(1024) NOT NULL,
    secret character varying(1024) NOT NULL
);


--
-- Name: used_username; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.used_username (
    username character varying(128) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL
);


--
-- Name: user; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public."user" (
    id character varying(32) NOT NULL,
    "updatedAt" timestamp with time zone,
    "lastFetchedAt" timestamp with time zone,
    username character varying(128) NOT NULL,
    "usernameLower" character varying(128) NOT NULL,
    name character varying(128),
    "followersCount" integer DEFAULT 0 NOT NULL,
    "followingCount" integer DEFAULT 0 NOT NULL,
    "notesCount" integer DEFAULT 0 NOT NULL,
    "avatarId" character varying(32),
    "bannerId" character varying(32),
    tags character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    "isSuspended" boolean DEFAULT false NOT NULL,
    "isLocked" boolean DEFAULT false NOT NULL,
    "isBot" boolean DEFAULT false NOT NULL,
    "isCat" boolean DEFAULT false NOT NULL,
    emojis character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    host character varying(128),
    inbox character varying(512),
    "sharedInbox" character varying(512),
    featured character varying(512),
    uri character varying(512),
    token character(16),
    "isExplorable" boolean DEFAULT true NOT NULL,
    "followersUri" character varying(512),
    "lastActiveDate" timestamp with time zone,
    "hideOnlineStatus" boolean DEFAULT false NOT NULL,
    "isDeleted" boolean DEFAULT false NOT NULL,
    "avatarUrl" character varying(1024),
    "bannerUrl" character varying(512),
    "avatarBlurhash" character varying(128),
    "bannerBlurhash" character varying(128),
    "movedToUri" character varying(512),
    "alsoKnownAs" text,
    "movedAt" timestamp with time zone,
    "isHibernated" boolean DEFAULT false NOT NULL,
    "avatarDecorations" jsonb DEFAULT '[]'::jsonb NOT NULL,
    score integer DEFAULT 0 NOT NULL,
    "requireSigninToViewContents" boolean DEFAULT false NOT NULL,
    "makeNotesFollowersOnlyBefore" integer,
    "makeNotesHiddenBefore" integer,
    "chatScope" character varying(128) DEFAULT 'mutual'::character varying NOT NULL
);


--
-- Name: COLUMN "user"."updatedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."updatedAt" IS 'The updated date of the User.';


--
-- Name: COLUMN "user".username; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".username IS 'The username of the User.';


--
-- Name: COLUMN "user"."usernameLower"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."usernameLower" IS 'The username (lowercased) of the User.';


--
-- Name: COLUMN "user".name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".name IS 'The name of the User.';


--
-- Name: COLUMN "user"."followersCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."followersCount" IS 'The count of followers.';


--
-- Name: COLUMN "user"."followingCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."followingCount" IS 'The count of following.';


--
-- Name: COLUMN "user"."notesCount"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."notesCount" IS 'The count of notes.';


--
-- Name: COLUMN "user"."avatarId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."avatarId" IS 'The ID of avatar DriveFile.';


--
-- Name: COLUMN "user"."bannerId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."bannerId" IS 'The ID of banner DriveFile.';


--
-- Name: COLUMN "user"."isSuspended"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."isSuspended" IS 'Whether the User is suspended.';


--
-- Name: COLUMN "user"."isLocked"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."isLocked" IS 'Whether the User is locked.';


--
-- Name: COLUMN "user"."isBot"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."isBot" IS 'Whether the User is a bot.';


--
-- Name: COLUMN "user"."isCat"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."isCat" IS 'Whether the User is a cat.';


--
-- Name: COLUMN "user".host; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".host IS 'The host of the User. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user".inbox; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".inbox IS 'The inbox URL of the User. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user"."sharedInbox"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."sharedInbox" IS 'The sharedInbox URL of the User. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user".featured; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".featured IS 'The featured URL of the User. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user".uri; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".uri IS 'The URI of the User. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user".token; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user".token IS 'The native access token of the User. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user"."isExplorable"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."isExplorable" IS 'Whether the User is explorable.';


--
-- Name: COLUMN "user"."followersUri"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."followersUri" IS 'The URI of the user Follower Collection. It will be null if the origin of the user is local.';


--
-- Name: COLUMN "user"."isDeleted"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."isDeleted" IS 'Whether the User is deleted.';


--
-- Name: COLUMN "user"."movedToUri"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."movedToUri" IS 'The URI of the new account of the User';


--
-- Name: COLUMN "user"."alsoKnownAs"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."alsoKnownAs" IS 'URIs the user is known as too';


--
-- Name: COLUMN "user"."movedAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public."user"."movedAt" IS 'When the user moved to another account';


--
-- Name: user_group; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_group (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    name character varying(256) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "isPrivate" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN user_group."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group."createdAt" IS 'The created date of the UserGroup.';


--
-- Name: COLUMN user_group."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group."userId" IS 'The ID of owner.';


--
-- Name: user_group_invitation; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_group_invitation (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    "userGroupId" character varying(32) NOT NULL
);


--
-- Name: COLUMN user_group_invitation."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group_invitation."createdAt" IS 'The created date of the UserGroupInvitation.';


--
-- Name: COLUMN user_group_invitation."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group_invitation."userId" IS 'The user ID.';


--
-- Name: COLUMN user_group_invitation."userGroupId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group_invitation."userGroupId" IS 'The group ID.';


--
-- Name: user_group_invite; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_group_invite (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    "userGroupId" character varying(32) NOT NULL
);


--
-- Name: user_group_joining; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_group_joining (
    id character varying(32) NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    "userGroupId" character varying(32) NOT NULL
);


--
-- Name: COLUMN user_group_joining."createdAt"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group_joining."createdAt" IS 'The created date of the UserGroupJoining.';


--
-- Name: COLUMN user_group_joining."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group_joining."userId" IS 'The user ID.';


--
-- Name: COLUMN user_group_joining."userGroupId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_group_joining."userGroupId" IS 'The group ID.';


--
-- Name: user_ip; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_ip (
    id integer NOT NULL,
    "createdAt" timestamp with time zone NOT NULL,
    "userId" character varying(32) NOT NULL,
    ip character varying(128) NOT NULL
);


--
-- Name: user_ip_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.user_ip_id_seq
    AS integer
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: user_ip_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.user_ip_id_seq OWNED BY public.user_ip.id;


--
-- Name: user_keypair; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_keypair (
    "userId" character varying(32) NOT NULL,
    "publicKey" character varying(4096) NOT NULL,
    "privateKey" character varying(4096) NOT NULL
);


--
-- Name: user_list; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_list (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    name character varying(128) NOT NULL,
    "isPublic" boolean DEFAULT false NOT NULL
);


--
-- Name: COLUMN user_list."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_list."userId" IS 'The owner ID.';


--
-- Name: COLUMN user_list.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_list.name IS 'The name of the UserList.';


--
-- Name: user_list_favorite; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_list_favorite (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "userListId" character varying(32) NOT NULL
);


--
-- Name: user_list_membership; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_list_membership (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "userListId" character varying(32) NOT NULL,
    "withReplies" boolean DEFAULT false NOT NULL,
    "userListUserId" character varying(32) NOT NULL
);


--
-- Name: COLUMN user_list_membership."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_list_membership."userId" IS 'The user ID.';


--
-- Name: COLUMN user_list_membership."userListId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_list_membership."userListId" IS 'The list ID.';


--
-- Name: user_memo; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_memo (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "targetUserId" character varying(32) NOT NULL,
    memo character varying(2048) NOT NULL
);


--
-- Name: COLUMN user_memo."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_memo."userId" IS 'The ID of author.';


--
-- Name: COLUMN user_memo."targetUserId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_memo."targetUserId" IS 'The ID of target user.';


--
-- Name: COLUMN user_memo.memo; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_memo.memo IS 'Memo.';


--
-- Name: user_note_pining; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_note_pining (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    "noteId" character varying(32) NOT NULL
);


--
-- Name: user_pending; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_pending (
    id character varying(32) NOT NULL,
    code character varying(128) NOT NULL,
    username character varying(128) NOT NULL,
    email character varying(128) NOT NULL,
    password character varying(128) NOT NULL
);


--
-- Name: user_profile; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_profile (
    "userId" character varying(32) NOT NULL,
    location character varying(128),
    birthday character(10),
    description character varying(2048),
    fields jsonb DEFAULT '[]'::jsonb NOT NULL,
    url character varying(512),
    email character varying(128),
    "emailVerifyCode" character varying(128),
    "emailVerified" boolean DEFAULT false NOT NULL,
    "twoFactorTempSecret" character varying(128),
    "twoFactorSecret" character varying(128),
    "twoFactorEnabled" boolean DEFAULT false NOT NULL,
    password character varying(128),
    "clientData" jsonb DEFAULT '{}'::jsonb NOT NULL,
    "autoAcceptFollowed" boolean DEFAULT false NOT NULL,
    "alwaysMarkNsfw" boolean DEFAULT false NOT NULL,
    "carefulBot" boolean DEFAULT false NOT NULL,
    "userHost" character varying(128),
    "securityKeysAvailable" boolean DEFAULT false NOT NULL,
    "usePasswordLessLogin" boolean DEFAULT false NOT NULL,
    "pinnedPageId" character varying(32),
    room jsonb DEFAULT '{}'::jsonb NOT NULL,
    "injectFeaturedNote" boolean DEFAULT true NOT NULL,
    "enableWordMute" boolean DEFAULT false NOT NULL,
    "mutedWords" jsonb DEFAULT '[]'::jsonb NOT NULL,
    "noCrawle" boolean DEFAULT false NOT NULL,
    "receiveAnnouncementEmail" boolean DEFAULT true NOT NULL,
    "emailNotificationTypes" jsonb DEFAULT '["follow", "receiveFollowRequest"]'::jsonb NOT NULL,
    lang character varying(32),
    "mutedInstances" jsonb DEFAULT '[]'::jsonb NOT NULL,
    "publicReactions" boolean DEFAULT true NOT NULL,
    "autoSensitive" boolean DEFAULT false NOT NULL,
    "moderationNote" character varying(8192) DEFAULT ''::character varying NOT NULL,
    achievements jsonb DEFAULT '[]'::jsonb NOT NULL,
    "loggedInDates" character varying(32)[] DEFAULT '{}'::character varying[] NOT NULL,
    "preventAiLearning" boolean DEFAULT true NOT NULL,
    "twoFactorBackupSecret" character varying[],
    "verifiedLinks" character varying[] DEFAULT '{}'::character varying[] NOT NULL,
    "notificationRecieveConfig" jsonb DEFAULT '{}'::jsonb NOT NULL,
    "hardMutedWords" jsonb DEFAULT '[]'::jsonb NOT NULL,
    "followingVisibility" public.user_profile_followingvisibility_enum DEFAULT 'public'::public.user_profile_followingvisibility_enum NOT NULL,
    "followersVisibility" public.user_profile_followersvisibility_enum DEFAULT 'public'::public.user_profile_followersvisibility_enum NOT NULL,
    "followedMessage" character varying(256),
    "followApprovalLocalSeconds" integer,
    "followApprovalRemoteSeconds" integer
);


--
-- Name: COLUMN user_profile.location; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.location IS 'The location of the User.';


--
-- Name: COLUMN user_profile.birthday; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.birthday IS 'The birthday (YYYY-MM-DD) of the User.';


--
-- Name: COLUMN user_profile.description; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.description IS 'The description (bio) of the User.';


--
-- Name: COLUMN user_profile.url; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.url IS 'Remote URL of the user.';


--
-- Name: COLUMN user_profile.email; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.email IS 'The email address of the User.';


--
-- Name: COLUMN user_profile.password; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.password IS 'The password hash of the User. It will be null if the origin of the user is remote.';


--
-- Name: COLUMN user_profile."clientData"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile."clientData" IS 'The client-specific data of the User.';


--
-- Name: COLUMN user_profile."userHost"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile."userHost" IS '[Denormalized]';


--
-- Name: COLUMN user_profile.room; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile.room IS 'The room data of the User.';


--
-- Name: COLUMN user_profile."noCrawle"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile."noCrawle" IS 'Whether reject index by crawler.';


--
-- Name: COLUMN user_profile."mutedInstances"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_profile."mutedInstances" IS 'List of instances muted by the user.';


--
-- Name: user_publickey; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_publickey (
    "userId" character varying(32) NOT NULL,
    "keyId" character varying(256) NOT NULL,
    "keyPem" character varying(4096) NOT NULL
);


--
-- Name: user_security_key; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_security_key (
    id character varying NOT NULL,
    "userId" character varying(32) NOT NULL,
    "publicKey" character varying NOT NULL,
    "lastUsed" timestamp with time zone DEFAULT now() NOT NULL,
    name character varying(30) NOT NULL,
    counter bigint DEFAULT '0'::bigint NOT NULL,
    "credentialDeviceType" character varying(32),
    "credentialBackedUp" boolean,
    transports character varying(32)[]
);


--
-- Name: COLUMN user_security_key.id; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key.id IS 'Variable-length id given to navigator.credentials.get()';


--
-- Name: COLUMN user_security_key."publicKey"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key."publicKey" IS 'The public key of the UserSecurityKey, hex-encoded.';


--
-- Name: COLUMN user_security_key."lastUsed"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key."lastUsed" IS 'Timestamp of the last time the UserSecurityKey was used.';


--
-- Name: COLUMN user_security_key.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key.name IS 'User-defined name for this key';


--
-- Name: COLUMN user_security_key.counter; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key.counter IS 'The number of times the UserSecurityKey was validated.';


--
-- Name: COLUMN user_security_key."credentialDeviceType"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key."credentialDeviceType" IS 'The type of Backup Eligibility in authenticator data';


--
-- Name: COLUMN user_security_key."credentialBackedUp"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key."credentialBackedUp" IS 'Whether or not the credential has been backed up';


--
-- Name: COLUMN user_security_key.transports; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.user_security_key.transports IS 'The type of the credential returned by the browser';


--
-- Name: webhook; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook (
    id character varying(32) NOT NULL,
    "userId" character varying(32) NOT NULL,
    name character varying(128) NOT NULL,
    "on" character varying(128)[] DEFAULT '{}'::character varying[] NOT NULL,
    url character varying(1024) NOT NULL,
    secret character varying(1024) NOT NULL,
    active boolean DEFAULT true NOT NULL,
    "latestSentAt" timestamp with time zone,
    "latestStatus" integer
);


--
-- Name: COLUMN webhook."userId"; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.webhook."userId" IS 'The owner ID.';


--
-- Name: COLUMN webhook.name; Type: COMMENT; Schema: public; Owner: -
--

COMMENT ON COLUMN public.webhook.name IS 'The name of the Antenna.';


--
-- Name: __chart__active_users id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__active_users ALTER COLUMN id SET DEFAULT nextval('public.__chart__active_users_id_seq'::regclass);


--
-- Name: __chart__ap_request id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__ap_request ALTER COLUMN id SET DEFAULT nextval('public.__chart__ap_request_id_seq'::regclass);


--
-- Name: __chart__drive id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__drive ALTER COLUMN id SET DEFAULT nextval('public.__chart__drive_id_seq'::regclass);


--
-- Name: __chart__federation id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__federation ALTER COLUMN id SET DEFAULT nextval('public.__chart__federation_id_seq'::regclass);


--
-- Name: __chart__hashtag id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__hashtag ALTER COLUMN id SET DEFAULT nextval('public.__chart__hashtag_id_seq'::regclass);


--
-- Name: __chart__instance id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__instance ALTER COLUMN id SET DEFAULT nextval('public.__chart__instance_id_seq'::regclass);


--
-- Name: __chart__network id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__network ALTER COLUMN id SET DEFAULT nextval('public.__chart__network_id_seq'::regclass);


--
-- Name: __chart__notes id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__notes ALTER COLUMN id SET DEFAULT nextval('public.__chart__notes_id_seq'::regclass);


--
-- Name: __chart__per_user_drive id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_drive ALTER COLUMN id SET DEFAULT nextval('public.__chart__per_user_drive_id_seq'::regclass);


--
-- Name: __chart__per_user_following id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_following ALTER COLUMN id SET DEFAULT nextval('public.__chart__per_user_following_id_seq'::regclass);


--
-- Name: __chart__per_user_notes id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_notes ALTER COLUMN id SET DEFAULT nextval('public.__chart__per_user_notes_id_seq'::regclass);


--
-- Name: __chart__per_user_pv id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_pv ALTER COLUMN id SET DEFAULT nextval('public.__chart__per_user_pv_id_seq'::regclass);


--
-- Name: __chart__per_user_reaction id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_reaction ALTER COLUMN id SET DEFAULT nextval('public.__chart__per_user_reaction_id_seq'::regclass);


--
-- Name: __chart__test id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__test ALTER COLUMN id SET DEFAULT nextval('public.__chart__test_id_seq'::regclass);


--
-- Name: __chart__test_grouped id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__test_grouped ALTER COLUMN id SET DEFAULT nextval('public.__chart__test_grouped_id_seq'::regclass);


--
-- Name: __chart__test_unique id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__test_unique ALTER COLUMN id SET DEFAULT nextval('public.__chart__test_unique_id_seq'::regclass);


--
-- Name: __chart__users id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__users ALTER COLUMN id SET DEFAULT nextval('public.__chart__users_id_seq'::regclass);


--
-- Name: __chart_day__active_users id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__active_users ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__active_users_id_seq'::regclass);


--
-- Name: __chart_day__ap_request id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__ap_request ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__ap_request_id_seq'::regclass);


--
-- Name: __chart_day__drive id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__drive ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__drive_id_seq'::regclass);


--
-- Name: __chart_day__federation id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__federation ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__federation_id_seq'::regclass);


--
-- Name: __chart_day__hashtag id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__hashtag ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__hashtag_id_seq'::regclass);


--
-- Name: __chart_day__instance id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__instance ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__instance_id_seq'::regclass);


--
-- Name: __chart_day__network id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__network ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__network_id_seq'::regclass);


--
-- Name: __chart_day__notes id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__notes ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__notes_id_seq'::regclass);


--
-- Name: __chart_day__per_user_drive id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_drive ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__per_user_drive_id_seq'::regclass);


--
-- Name: __chart_day__per_user_following id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_following ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__per_user_following_id_seq'::regclass);


--
-- Name: __chart_day__per_user_notes id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_notes ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__per_user_notes_id_seq'::regclass);


--
-- Name: __chart_day__per_user_pv id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_pv ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__per_user_pv_id_seq'::regclass);


--
-- Name: __chart_day__per_user_reaction id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_reaction ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__per_user_reaction_id_seq'::regclass);


--
-- Name: __chart_day__users id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__users ALTER COLUMN id SET DEFAULT nextval('public.__chart_day__users_id_seq'::regclass);


--
-- Name: user_ip id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_ip ALTER COLUMN id SET DEFAULT nextval('public.user_ip_id_seq'::regclass);


--
-- Name: __chart_day__per_user_pv PK_0085d7542f6772e99b9dcfb0a9c; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_pv
    ADD CONSTRAINT "PK_0085d7542f6772e99b9dcfb0a9c" PRIMARY KEY (id);


--
-- Name: ad PK_0193d5ef09746e88e9ea92c634d; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ad
    ADD CONSTRAINT "PK_0193d5ef09746e88e9ea92c634d" PRIMARY KEY (id);


--
-- Name: __chart__notes PK_0aec823fa85c7f901bdb3863b14; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__notes
    ADD CONSTRAINT "PK_0aec823fa85c7f901bdb3863b14" PRIMARY KEY (id);


--
-- Name: flash PK_0c01a2c1c5f2266942dd1b3fdbc; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flash
    ADD CONSTRAINT "PK_0c01a2c1c5f2266942dd1b3fdbc" PRIMARY KEY (id);


--
-- Name: user_publickey PK_10c146e4b39b443ede016f6736d; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_publickey
    ADD CONSTRAINT "PK_10c146e4b39b443ede016f6736d" PRIMARY KEY ("userId");


--
-- Name: user_list_membership PK_11abb3768da1c5f8de101c9df45; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list_membership
    ADD CONSTRAINT "PK_11abb3768da1c5f8de101c9df45" PRIMARY KEY (id);


--
-- Name: __chart__instance PK_1267c67c7c2d47b4903975f2c00; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__instance
    ADD CONSTRAINT "PK_1267c67c7c2d47b4903975f2c00" PRIMARY KEY (id);


--
-- Name: __chart_day__hashtag PK_13d5a3b089344e5557f8e0980b4; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__hashtag
    ADD CONSTRAINT "PK_13d5a3b089344e5557f8e0980b4" PRIMARY KEY (id);


--
-- Name: user_group_joining PK_15f2425885253c5507e1599cfe7; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_joining
    ADD CONSTRAINT "PK_15f2425885253c5507e1599cfe7" PRIMARY KEY (id);


--
-- Name: user_group_invitation PK_160c63ec02bf23f6a5c5e8140d6; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_invitation
    ADD CONSTRAINT "PK_160c63ec02bf23f6a5c5e8140d6" PRIMARY KEY (id);


--
-- Name: note_unread PK_1904eda61a784f57e6e51fa9c1f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_unread
    ADD CONSTRAINT "PK_1904eda61a784f57e6e51fa9c1f" PRIMARY KEY (id);


--
-- Name: auth_session PK_19354ed146424a728c1112a8cbf; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.auth_session
    ADD CONSTRAINT "PK_19354ed146424a728c1112a8cbf" PRIMARY KEY (id);


--
-- Name: __chart_day__per_user_drive PK_1ae135254c137011645da7f4045; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_drive
    ADD CONSTRAINT "PK_1ae135254c137011645da7f4045" PRIMARY KEY (id);


--
-- Name: clip_favorite PK_1b539f43906f05ebcabe752a977; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip_favorite
    ADD CONSTRAINT "PK_1b539f43906f05ebcabe752a977" PRIMARY KEY (id);


--
-- Name: __chart_day__notes PK_1fa4139e1f338272b758d05e090; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__notes
    ADD CONSTRAINT "PK_1fa4139e1f338272b758d05e090" PRIMARY KEY (id);


--
-- Name: retention_aggregation PK_22aad3e8640b15fb3b90ee02d18; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.retention_aggregation
    ADD CONSTRAINT "PK_22aad3e8640b15fb3b90ee02d18" PRIMARY KEY (id);


--
-- Name: chat_room_membership PK_2bd59c741e571b283c048beb69a; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room_membership
    ADD CONSTRAINT "PK_2bd59c741e571b283c048beb69a" PRIMARY KEY (id);


--
-- Name: user_ip PK_2c44ddfbf7c0464d028dcef325e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_ip
    ADD CONSTRAINT "PK_2c44ddfbf7c0464d028dcef325e" PRIMARY KEY (id);


--
-- Name: muting PK_2e92d06c8b5c602eeb27ca9ba48; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.muting
    ADD CONSTRAINT "PK_2e92d06c8b5c602eeb27ca9ba48" PRIMARY KEY (id);


--
-- Name: __chart__active_users PK_317237a9f733b970604a11e314f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__active_users
    ADD CONSTRAINT "PK_317237a9f733b970604a11e314f" PRIMARY KEY (id);


--
-- Name: __chart__per_user_notes PK_334acf6e915af2f29edc11b8e50; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_notes
    ADD CONSTRAINT "PK_334acf6e915af2f29edc11b8e50" PRIMARY KEY (id);


--
-- Name: user_group_invite PK_3893884af0d3a5f4d01e7921a97; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_invite
    ADD CONSTRAINT "PK_3893884af0d3a5f4d01e7921a97" PRIMARY KEY (id);


--
-- Name: user_group PK_3c29fba6fe013ec8724378ce7c9; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group
    ADD CONSTRAINT "PK_3c29fba6fe013ec8724378ce7c9" PRIMARY KEY (id);


--
-- Name: __chart__per_user_pv PK_3c938a24f0203b5bd13fab51059; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_pv
    ADD CONSTRAINT "PK_3c938a24f0203b5bd13fab51059" PRIMARY KEY (id);


--
-- Name: chat_message PK_3cc0d85193aade457d3077dd06b; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message
    ADD CONSTRAINT "PK_3cc0d85193aade457d3077dd06b" PRIMARY KEY (id);


--
-- Name: user_security_key PK_3e508571121ab39c5f85d10c166; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_security_key
    ADD CONSTRAINT "PK_3e508571121ab39c5f85d10c166" PRIMARY KEY (id);


--
-- Name: __chart__test_unique PK_409bac9c97cc612d8500012319d; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__test_unique
    ADD CONSTRAINT "PK_409bac9c97cc612d8500012319d" PRIMARY KEY (id);


--
-- Name: drive_file PK_43ddaaaf18c9e68029b7cbb032e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.drive_file
    ADD CONSTRAINT "PK_43ddaaaf18c9e68029b7cbb032e" PRIMARY KEY (id);


--
-- Name: channel_note_pining PK_44f7474496bcf2e4b741681146d; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_note_pining
    ADD CONSTRAINT "PK_44f7474496bcf2e4b741681146d" PRIMARY KEY (id);


--
-- Name: __chart_day__instance PK_479a8ff9d959274981087043023; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__instance
    ADD CONSTRAINT "PK_479a8ff9d959274981087043023" PRIMARY KEY (id);


--
-- Name: note_watching PK_49286fdb23725945a74aa27d757; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_watching
    ADD CONSTRAINT "PK_49286fdb23725945a74aa27d757" PRIMARY KEY (id);


--
-- Name: announcement_read PK_4b90ad1f42681d97b2683890c5e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.announcement_read
    ADD CONSTRAINT "PK_4b90ad1f42681d97b2683890c5e" PRIMARY KEY (id);


--
-- Name: __chart__users PK_4dfcf2c78d03524b9eb2c99d328; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__users
    ADD CONSTRAINT "PK_4dfcf2c78d03524b9eb2c99d328" PRIMARY KEY (id);


--
-- Name: user_profile PK_51cb79b5555effaf7d69ba1cff9; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_profile
    ADD CONSTRAINT "PK_51cb79b5555effaf7d69ba1cff9" PRIMARY KEY ("userId");


--
-- Name: follow_request PK_53a9aa3725f7a3deb150b39dbfc; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.follow_request
    ADD CONSTRAINT "PK_53a9aa3725f7a3deb150b39dbfc" PRIMARY KEY (id);


--
-- Name: __chart__ap_request PK_56a25cd447c7ee08876b3baf8d8; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__ap_request
    ADD CONSTRAINT "PK_56a25cd447c7ee08876b3baf8d8" PRIMARY KEY (id);


--
-- Name: __chart_day__per_user_notes PK_58bab6b6d3ad9310cbc7460fd28; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_notes
    ADD CONSTRAINT "PK_58bab6b6d3ad9310cbc7460fd28" PRIMARY KEY (id);


--
-- Name: channel PK_590f33ee6ee7d76437acf362e39; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel
    ADD CONSTRAINT "PK_590f33ee6ee7d76437acf362e39" PRIMARY KEY (id);


--
-- Name: channel_favorite PK_59bddfd54d48689a298d41af00c; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_favorite
    ADD CONSTRAINT "PK_59bddfd54d48689a298d41af00c" PRIMARY KEY (id);


--
-- Name: promo_read PK_61917c1541002422b703318b7c9; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.promo_read
    ADD CONSTRAINT "PK_61917c1541002422b703318b7c9" PRIMARY KEY (id);


--
-- Name: registry_item PK_64b3f7e6008b4d89b826cd3af95; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.registry_item
    ADD CONSTRAINT "PK_64b3f7e6008b4d89b826cd3af95" PRIMARY KEY (id);


--
-- Name: __chart_day__per_user_following PK_68ce6b67da57166da66fc8fb27e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_following
    ADD CONSTRAINT "PK_68ce6b67da57166da66fc8fb27e" PRIMARY KEY (id);


--
-- Name: page PK_742f4117e065c5b6ad21b37ba1f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.page
    ADD CONSTRAINT "PK_742f4117e065c5b6ad21b37ba1f" PRIMARY KEY (id);


--
-- Name: note_reaction PK_767ec729b108799b587a3fcc9cf; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_reaction
    ADD CONSTRAINT "PK_767ec729b108799b587a3fcc9cf" PRIMARY KEY (id);


--
-- Name: reversi_game PK_76b30eeba71b1193ad7c5311c3f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reversi_game
    ADD CONSTRAINT "PK_76b30eeba71b1193ad7c5311c3f" PRIMARY KEY (id);


--
-- Name: relay PK_78ebc9cfddf4292633b7ba57aee; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.relay
    ADD CONSTRAINT "PK_78ebc9cfddf4292633b7ba57aee" PRIMARY KEY (id);


--
-- Name: used_username PK_78fd79d2d24c6ac2f4cc9a31a5d; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.used_username
    ADD CONSTRAINT "PK_78fd79d2d24c6ac2f4cc9a31a5d" PRIMARY KEY (username);


--
-- Name: drive_folder PK_7a0c089191f5ebdc214e0af808a; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.drive_folder
    ADD CONSTRAINT "PK_7a0c089191f5ebdc214e0af808a" PRIMARY KEY (id);


--
-- Name: __chart_day__federation PK_7ca721c769f31698e0e1331e8e6; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__federation
    ADD CONSTRAINT "PK_7ca721c769f31698e0e1331e8e6" PRIMARY KEY (id);


--
-- Name: role_assignment PK_7e79671a8a5db18936173148cb4; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role_assignment
    ADD CONSTRAINT "PK_7e79671a8a5db18936173148cb4" PRIMARY KEY (id);


--
-- Name: page_like PK_813f034843af992d3ae0f43c64c; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.page_like
    ADD CONSTRAINT "PK_813f034843af992d3ae0f43c64c" PRIMARY KEY (id);


--
-- Name: gallery_like PK_853ab02be39b8de45cd720cc15f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gallery_like
    ADD CONSTRAINT "PK_853ab02be39b8de45cd720cc15f" PRIMARY KEY (id);


--
-- Name: __chart__per_user_following PK_85bb1b540363a29c2fec83bd907; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_following
    ADD CONSTRAINT "PK_85bb1b540363a29c2fec83bd907" PRIMARY KEY (id);


--
-- Name: abuse_user_report PK_87873f5f5cc5c321a1306b2d18c; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_user_report
    ADD CONSTRAINT "PK_87873f5f5cc5c321a1306b2d18c" PRIMARY KEY (id);


--
-- Name: user_list PK_87bab75775fd9b1ff822b656402; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list
    ADD CONSTRAINT "PK_87bab75775fd9b1ff822b656402" PRIMARY KEY (id);


--
-- Name: reversi_matching PK_880bd0afbab232f21c8b9d146cf; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reversi_matching
    ADD CONSTRAINT "PK_880bd0afbab232f21c8b9d146cf" PRIMARY KEY (id);


--
-- Name: chat_room PK_8aa3a52cf74c96469f0ef9fbe3e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room
    ADD CONSTRAINT "PK_8aa3a52cf74c96469f0ef9fbe3e" PRIMARY KEY (id);


--
-- Name: __chart_day__per_user_reaction PK_8af24e2d51ff781a354fe595eda; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_reaction
    ADD CONSTRAINT "PK_8af24e2d51ff781a354fe595eda" PRIMARY KEY (id);


--
-- Name: channel_following PK_8b104be7f7415113f2a02cd5bdd; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_following
    ADD CONSTRAINT "PK_8b104be7f7415113f2a02cd5bdd" PRIMARY KEY (id);


--
-- Name: gallery_post PK_8e90d7b6015f2c4518881b14753; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gallery_post
    ADD CONSTRAINT "PK_8e90d7b6015f2c4518881b14753" PRIMARY KEY (id);


--
-- Name: __chart_day__ap_request PK_9318b49daee320194e23f712e69; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__ap_request
    ADD CONSTRAINT "PK_9318b49daee320194e23f712e69" PRIMARY KEY (id);


--
-- Name: app PK_9478629fc093d229df09e560aea; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.app
    ADD CONSTRAINT "PK_9478629fc093d229df09e560aea" PRIMARY KEY (id);


--
-- Name: note PK_96d0c172a4fba276b1bbed43058; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note
    ADD CONSTRAINT "PK_96d0c172a4fba276b1bbed43058" PRIMARY KEY (id);


--
-- Name: __chart__per_user_reaction PK_984f54dae441e65b633e8d27a7f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_reaction
    ADD CONSTRAINT "PK_984f54dae441e65b633e8d27a7f" PRIMARY KEY (id);


--
-- Name: chat_room_invitation PK_9d489521a312dd28225672de2dc; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room_invitation
    ADD CONSTRAINT "PK_9d489521a312dd28225672de2dc" PRIMARY KEY (id);


--
-- Name: signin PK_9e96ddc025712616fc492b3b588; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.signin
    ADD CONSTRAINT "PK_9e96ddc025712616fc492b3b588" PRIMARY KEY (id);


--
-- Name: user_note_pining PK_a6a2dad4ae000abce2ea9d9b103; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_note_pining
    ADD CONSTRAINT "PK_a6a2dad4ae000abce2ea9d9b103" PRIMARY KEY (id);


--
-- Name: bubble_game_record PK_a75395fe404b392e2893b50d7ea; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bubble_game_record
    ADD CONSTRAINT "PK_a75395fe404b392e2893b50d7ea" PRIMARY KEY (id);


--
-- Name: abuse_report_notification_recipient PK_abuse_report_notification_recipient_id; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_report_notification_recipient
    ADD CONSTRAINT "PK_abuse_report_notification_recipient_id" PRIMARY KEY (id);


--
-- Name: channel_muting PK_aec842e98f332ebd8e12f85bad6; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_muting
    ADD CONSTRAINT "PK_aec842e98f332ebd8e12f85bad6" PRIMARY KEY (id);


--
-- Name: note_favorite PK_af0da35a60b9fa4463a62082b36; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_favorite
    ADD CONSTRAINT "PK_af0da35a60b9fa4463a62082b36" PRIMARY KEY (id);


--
-- Name: __chart_day__active_users PK_b1790489b14f005ae8f404f5795; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__active_users
    ADD CONSTRAINT "PK_b1790489b14f005ae8f404f5795" PRIMARY KEY (id);


--
-- Name: role PK_b36bcfe02fc8de3c57a8b2391c2; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role
    ADD CONSTRAINT "PK_b36bcfe02fc8de3c57a8b2391c2" PRIMARY KEY (id);


--
-- Name: __chart__federation PK_b39dcd31a0fe1a7757e348e85fd; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__federation
    ADD CONSTRAINT "PK_b39dcd31a0fe1a7757e348e85fd" PRIMARY KEY (id);


--
-- Name: __chart__test PK_b4bc31dffbd1b785276a3ecfc1e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__test
    ADD CONSTRAINT "PK_b4bc31dffbd1b785276a3ecfc1e" PRIMARY KEY (id);


--
-- Name: avatar_decoration PK_b6de9296f6097078e1dc53f7603; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.avatar_decoration
    ADD CONSTRAINT "PK_b6de9296f6097078e1dc53f7603" PRIMARY KEY (id);


--
-- Name: __chart__network PK_bc4290c2e27fad14ef0c1ca93f3; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__network
    ADD CONSTRAINT "PK_bc4290c2e27fad14ef0c1ca93f3" PRIMARY KEY (id);


--
-- Name: user_list_favorite PK_c0974b21e18502a4c8178e09fe6; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list_favorite
    ADD CONSTRAINT "PK_c0974b21e18502a4c8178e09fe6" PRIMARY KEY (id);


--
-- Name: antenna PK_c170b99775e1dccca947c9f2d5f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.antenna
    ADD CONSTRAINT "PK_c170b99775e1dccca947c9f2d5f" PRIMARY KEY (id);


--
-- Name: __chart__hashtag PK_c32f1ea2b44a5d2f7881e37f8f9; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__hashtag
    ADD CONSTRAINT "PK_c32f1ea2b44a5d2f7881e37f8f9" PRIMARY KEY (id);


--
-- Name: meta PK_c4c17a6c2bd7651338b60fc590b; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.meta
    ADD CONSTRAINT "PK_c4c17a6c2bd7651338b60fc590b" PRIMARY KEY (id);


--
-- Name: following PK_c76c6e044bdf76ecf8bfb82a645; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.following
    ADD CONSTRAINT "PK_c76c6e044bdf76ecf8bfb82a645" PRIMARY KEY (id);


--
-- Name: __chart_day__network PK_cac499d6f471042dfed1e7e0132; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__network
    ADD CONSTRAINT "PK_cac499d6f471042dfed1e7e0132" PRIMARY KEY (id);


--
-- Name: user PK_cace4a159ff9f2512dd42373760; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT "PK_cace4a159ff9f2512dd42373760" PRIMARY KEY (id);


--
-- Name: hashtag PK_cb36eb8af8412bfa978f1165d78; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.hashtag
    ADD CONSTRAINT "PK_cb36eb8af8412bfa978f1165d78" PRIMARY KEY (id);


--
-- Name: moderation_log PK_d0adca6ecfd068db83e4526cc26; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.moderation_log
    ADD CONSTRAINT "PK_d0adca6ecfd068db83e4526cc26" PRIMARY KEY (id);


--
-- Name: __chart__per_user_drive PK_d0ef23d24d666e1a44a0cd3d208; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_drive
    ADD CONSTRAINT "PK_d0ef23d24d666e1a44a0cd3d208" PRIMARY KEY (id);


--
-- Name: flash_like PK_d110109ee310588d63d6183b233; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flash_like
    ADD CONSTRAINT "PK_d110109ee310588d63d6183b233" PRIMARY KEY (id);


--
-- Name: user_pending PK_d4c84e013c98ec02d19b8fbbafa; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_pending
    ADD CONSTRAINT "PK_d4c84e013c98ec02d19b8fbbafa" PRIMARY KEY (id);


--
-- Name: __chart_day__users PK_d7f7185abb9851f70c4726c54bd; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__users
    ADD CONSTRAINT "PK_d7f7185abb9851f70c4726c54bd" PRIMARY KEY (id);


--
-- Name: poll PK_da851e06d0dfe2ef397d8b1bf1b; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.poll
    ADD CONSTRAINT "PK_da851e06d0dfe2ef397d8b1bf1b" PRIMARY KEY ("noteId");


--
-- Name: messaging_message PK_db398fd79dc95d0eb8c30456eaa; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.messaging_message
    ADD CONSTRAINT "PK_db398fd79dc95d0eb8c30456eaa" PRIMARY KEY (id);


--
-- Name: emoji PK_df74ce05e24999ee01ea0bc50a3; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.emoji
    ADD CONSTRAINT "PK_df74ce05e24999ee01ea0bc50a3" PRIMARY KEY (id);


--
-- Name: announcement PK_e0ef0550174fd1099a308fd18a0; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.announcement
    ADD CONSTRAINT "PK_e0ef0550174fd1099a308fd18a0" PRIMARY KEY (id);


--
-- Name: promo_note PK_e263909ca4fe5d57f8d4230dd5c; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.promo_note
    ADD CONSTRAINT "PK_e263909ca4fe5d57f8d4230dd5c" PRIMARY KEY ("noteId");


--
-- Name: blocking PK_e5d9a541cc1965ee7e048ea09dd; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.blocking
    ADD CONSTRAINT "PK_e5d9a541cc1965ee7e048ea09dd" PRIMARY KEY (id);


--
-- Name: webhook PK_e6765510c2d078db49632b59020; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook
    ADD CONSTRAINT "PK_e6765510c2d078db49632b59020" PRIMARY KEY (id);


--
-- Name: __chart_day__drive PK_e7ec0de057c77c40fc8d8b62151; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__drive
    ADD CONSTRAINT "PK_e7ec0de057c77c40fc8d8b62151" PRIMARY KEY (id);


--
-- Name: sw_subscription PK_e8f763631530051b95eb6279b91; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sw_subscription
    ADD CONSTRAINT "PK_e8f763631530051b95eb6279b91" PRIMARY KEY (id);


--
-- Name: clip_note PK_e94cda2f40a99b57e032a1a738b; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip_note
    ADD CONSTRAINT "PK_e94cda2f40a99b57e032a1a738b" PRIMARY KEY (id);


--
-- Name: user_memo PK_e9aaa58f7d3699a84d79078f4d9; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_memo
    ADD CONSTRAINT "PK_e9aaa58f7d3699a84d79078f4d9" PRIMARY KEY (id);


--
-- Name: instance PK_eaf60e4a0c399c9935413e06474; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance
    ADD CONSTRAINT "PK_eaf60e4a0c399c9935413e06474" PRIMARY KEY (id);


--
-- Name: note_thread_muting PK_ec5936d94d1a0369646d12a3a47; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_thread_muting
    ADD CONSTRAINT "PK_ec5936d94d1a0369646d12a3a47" PRIMARY KEY (id);


--
-- Name: system_account PK_edb56f4aaf9ddd50ee556da97ba; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.system_account
    ADD CONSTRAINT "PK_edb56f4aaf9ddd50ee556da97ba" PRIMARY KEY (id);


--
-- Name: clip PK_f0685dac8d4dd056d7255670b75; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip
    ADD CONSTRAINT "PK_f0685dac8d4dd056d7255670b75" PRIMARY KEY (id);


--
-- Name: registration_ticket PK_f11696b6fafcf3662d4292734f8; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.registration_ticket
    ADD CONSTRAINT "PK_f11696b6fafcf3662d4292734f8" PRIMARY KEY (id);


--
-- Name: access_token PK_f20f028607b2603deabd8182d12; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_token
    ADD CONSTRAINT "PK_f20f028607b2603deabd8182d12" PRIMARY KEY (id);


--
-- Name: user_keypair PK_f4853eb41ab722fe05f81cedeb6; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_keypair
    ADD CONSTRAINT "PK_f4853eb41ab722fe05f81cedeb6" PRIMARY KEY ("userId");


--
-- Name: __chart__test_grouped PK_f4a2b175d308695af30d4293272; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__test_grouped
    ADD CONSTRAINT "PK_f4a2b175d308695af30d4293272" PRIMARY KEY (id);


--
-- Name: __chart__drive PK_f96bc548a765cd4b3b354221ce7; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__drive
    ADD CONSTRAINT "PK_f96bc548a765cd4b3b354221ce7" PRIMARY KEY (id);


--
-- Name: chat_approval PK_fbbb95d60acf5c85388345b5f5d; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_approval
    ADD CONSTRAINT "PK_fbbb95d60acf5c85388345b5f5d" PRIMARY KEY (id);


--
-- Name: password_reset_request PK_fcf4b02eae1403a2edaf87fd074; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.password_reset_request
    ADD CONSTRAINT "PK_fcf4b02eae1403a2edaf87fd074" PRIMARY KEY (id);


--
-- Name: poll_vote PK_fd002d371201c472490ba89c6a0; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.poll_vote
    ADD CONSTRAINT "PK_fd002d371201c472490ba89c6a0" PRIMARY KEY (id);


--
-- Name: renote_muting PK_renoteMuting_id; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.renote_muting
    ADD CONSTRAINT "PK_renoteMuting_id" PRIMARY KEY (id);


--
-- Name: system_webhook PK_system_webhook_id; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.system_webhook
    ADD CONSTRAINT "PK_system_webhook_id" PRIMARY KEY (id);


--
-- Name: user REL_58f5c71eaab331645112cf8cfa; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT "REL_58f5c71eaab331645112cf8cfa" UNIQUE ("avatarId");


--
-- Name: user REL_afc64b53f8db3707ceb34eb28e; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT "REL_afc64b53f8db3707ceb34eb28e" UNIQUE ("bannerId");


--
-- Name: __chart__active_users UQ_0ad37b7ef50f4ddc84363d7ccca; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__active_users
    ADD CONSTRAINT "UQ_0ad37b7ef50f4ddc84363d7ccca" UNIQUE (date);


--
-- Name: __chart_day__drive UQ_0b60ebb3aa0065f10b0616c1171; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__drive
    ADD CONSTRAINT "UQ_0b60ebb3aa0065f10b0616c1171" UNIQUE (date);


--
-- Name: __chart__drive UQ_13565815f618a1ff53886c5b28a; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__drive
    ADD CONSTRAINT "UQ_13565815f618a1ff53886c5b28a" UNIQUE (date);


--
-- Name: __chart_day__notes UQ_1a527b423ad0858a1af5a056d43; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__notes
    ADD CONSTRAINT "UQ_1a527b423ad0858a1af5a056d43" UNIQUE (date);


--
-- Name: __chart__per_user_reaction UQ_229a41ad465f9205f1f57032910; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_reaction
    ADD CONSTRAINT "UQ_229a41ad465f9205f1f57032910" UNIQUE (date, "group");


--
-- Name: __chart__hashtag UQ_25a97c02003338124b2b75fdbc8; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__hashtag
    ADD CONSTRAINT "UQ_25a97c02003338124b2b75fdbc8" UNIQUE (date, "group");


--
-- Name: __chart__per_user_drive UQ_30bf67687f483ace115c5ca6429; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_drive
    ADD CONSTRAINT "UQ_30bf67687f483ace115c5ca6429" UNIQUE (date, "group");


--
-- Name: __chart__federation UQ_36cb699c49580d4e6c2e6159f97; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__federation
    ADD CONSTRAINT "UQ_36cb699c49580d4e6c2e6159f97" UNIQUE (date);


--
-- Name: __chart__instance UQ_39ee857ab2f23493037c6b66311; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__instance
    ADD CONSTRAINT "UQ_39ee857ab2f23493037c6b66311" UNIQUE (date, "group");


--
-- Name: __chart__notes UQ_42eb716a37d381cdf566192b2be; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__notes
    ADD CONSTRAINT "UQ_42eb716a37d381cdf566192b2be" UNIQUE (date);


--
-- Name: __chart__per_user_notes UQ_5048e9daccbbbc6d567bb142d34; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_notes
    ADD CONSTRAINT "UQ_5048e9daccbbbc6d567bb142d34" UNIQUE (date, "group");


--
-- Name: __chart_day__federation UQ_617a8fe225a6e701d89e02d2c74; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__federation
    ADD CONSTRAINT "UQ_617a8fe225a6e701d89e02d2c74" UNIQUE (date);


--
-- Name: __chart_day__per_user_drive UQ_62aa5047b5aec92524f24c701d7; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_drive
    ADD CONSTRAINT "UQ_62aa5047b5aec92524f24c701d7" UNIQUE (date, "group");


--
-- Name: user_profile UQ_6dc44f1ceb65b1e72bacef2ca27; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_profile
    ADD CONSTRAINT "UQ_6dc44f1ceb65b1e72bacef2ca27" UNIQUE ("pinnedPageId");


--
-- Name: __chart__users UQ_845254b3eaf708ae8a6cac30265; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__users
    ADD CONSTRAINT "UQ_845254b3eaf708ae8a6cac30265" UNIQUE (date);


--
-- Name: __chart_day__network UQ_8bfa548c2b31f9e07db113773ee; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__network
    ADD CONSTRAINT "UQ_8bfa548c2b31f9e07db113773ee" UNIQUE (date);


--
-- Name: __chart_day__hashtag UQ_8f589cf056ff51f09d6096f6450; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__hashtag
    ADD CONSTRAINT "UQ_8f589cf056ff51f09d6096f6450" UNIQUE (date, "group");


--
-- Name: __chart__network UQ_a1efd3e0048a5f2793a47360dc6; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__network
    ADD CONSTRAINT "UQ_a1efd3e0048a5f2793a47360dc6" UNIQUE (date);


--
-- Name: __chart_day__ap_request UQ_a848f66d6cec11980a5dd595822; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__ap_request
    ADD CONSTRAINT "UQ_a848f66d6cec11980a5dd595822" UNIQUE (date);


--
-- Name: user UQ_a854e557b1b14814750c7c7b0c9; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT "UQ_a854e557b1b14814750c7c7b0c9" UNIQUE (token);


--
-- Name: registration_ticket UQ_b6f93f2f30bdbb9a5ebdc7c7189; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.registration_ticket
    ADD CONSTRAINT "UQ_b6f93f2f30bdbb9a5ebdc7c7189" UNIQUE ("usedById");


--
-- Name: __chart__per_user_following UQ_b77d4dd9562c3a899d9a286fcd7; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_following
    ADD CONSTRAINT "UQ_b77d4dd9562c3a899d9a286fcd7" UNIQUE (date, "group");


--
-- Name: __chart_day__per_user_notes UQ_c5545d4b31cdc684034e33b81c3; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_notes
    ADD CONSTRAINT "UQ_c5545d4b31cdc684034e33b81c3" UNIQUE (date, "group");


--
-- Name: __chart_day__users UQ_cad6e07c20037f31cdba8a350c3; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__users
    ADD CONSTRAINT "UQ_cad6e07c20037f31cdba8a350c3" UNIQUE (date);


--
-- Name: __chart_day__per_user_reaction UQ_d54b653660d808b118e36c184c0; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_reaction
    ADD CONSTRAINT "UQ_d54b653660d808b118e36c184c0" UNIQUE (date, "group");


--
-- Name: __chart_day__active_users UQ_d5954f3df5e5e3bdfc3c03f3906; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__active_users
    ADD CONSTRAINT "UQ_d5954f3df5e5e3bdfc3c03f3906" UNIQUE (date);


--
-- Name: __chart_day__per_user_following UQ_e4849a3231f38281280ea4c0eee; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_following
    ADD CONSTRAINT "UQ_e4849a3231f38281280ea4c0eee" UNIQUE (date, "group");


--
-- Name: __chart__ap_request UQ_e56f4beac5746d44bc3e19c80d0; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__ap_request
    ADD CONSTRAINT "UQ_e56f4beac5746d44bc3e19c80d0" UNIQUE (date);


--
-- Name: __chart_day__per_user_pv UQ_f221e45cfac5bea0ce0f3149fbb; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__per_user_pv
    ADD CONSTRAINT "UQ_f221e45cfac5bea0ce0f3149fbb" UNIQUE (date, "group");


--
-- Name: __chart__per_user_pv UQ_f2a56da57921ca8439f45c1d95f; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart__per_user_pv
    ADD CONSTRAINT "UQ_f2a56da57921ca8439f45c1d95f" UNIQUE (date, "group");


--
-- Name: __chart_day__instance UQ_fea7c0278325a1a2492f2d6acbf; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.__chart_day__instance
    ADD CONSTRAINT "UQ_fea7c0278325a1a2492f2d6acbf" UNIQUE (date, "group");


--
-- Name: note_draft note_draft_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_draft
    ADD CONSTRAINT note_draft_pkey PRIMARY KEY (id);


--
-- Name: IDX_00ceffb0cdc238b3233294f08f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_00ceffb0cdc238b3233294f08f" ON public.drive_folder USING btree ("parentId");


--
-- Name: IDX_016f613dc4feb807e03e3e7da9; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_016f613dc4feb807e03e3e7da9" ON public.user_list_favorite USING btree ("userId");


--
-- Name: IDX_021015e6683570ae9f6b0c62be; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_021015e6683570ae9f6b0c62be" ON public.user_list_membership USING btree ("userId");


--
-- Name: IDX_03e7028ab8388a3f5e3ce2a861; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_03e7028ab8388a3f5e3ce2a861" ON public.note_watching USING btree ("noteId");


--
-- Name: IDX_044f2a7962b8ee5bbfaa02e8a3; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_044f2a7962b8ee5bbfaa02e8a3" ON public.chat_room_invitation USING btree ("userId", "roomId");


--
-- Name: IDX_04cc96756f89d0b7f9473e8cdf; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_04cc96756f89d0b7f9473e8cdf" ON public.abuse_user_report USING btree ("reporterId");


--
-- Name: IDX_05cca34b985d1b8edc1d1e28df; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_05cca34b985d1b8edc1d1e28df" ON public.gallery_post USING btree (tags);


--
-- Name: IDX_0610ebcfcfb4a18441a9bcdab2; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0610ebcfcfb4a18441a9bcdab2" ON public.poll USING btree ("userId");


--
-- Name: IDX_0627125f1a8a42c9a1929edb55; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0627125f1a8a42c9a1929edb55" ON public.blocking USING btree ("blockerId");


--
-- Name: IDX_084c2abb8948ef59a37dce6ac1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_084c2abb8948ef59a37dce6ac1" ON public.antenna USING btree ("lastUsedAt");


--
-- Name: IDX_094b86cd36bb805d1aa1e8cc9a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_094b86cd36bb805d1aa1e8cc9a" ON public.channel USING btree ("usersCount");


--
-- Name: IDX_0953deda7ce6e1448e935859e5; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0953deda7ce6e1448e935859e5" ON public.role_assignment USING btree ("userId", "roleId");


--
-- Name: IDX_09f4e5b9e4a2f268d3e284e4b3; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_09f4e5b9e4a2f268d3e284e4b3" ON public.retention_aggregation USING btree ("createdAt");


--
-- Name: IDX_0a72bdfcdb97c0eca11fe7ecad; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0a72bdfcdb97c0eca11fe7ecad" ON public.registry_item USING btree (domain);


--
-- Name: IDX_0ad37b7ef50f4ddc84363d7ccc; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0ad37b7ef50f4ddc84363d7ccc" ON public.__chart__active_users USING btree (date);


--
-- Name: IDX_0b03cbcd7e6a7ce068efa8ecc2; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0b03cbcd7e6a7ce068efa8ecc2" ON public.hashtag USING btree ("attachedRemoteUsersCount");


--
-- Name: IDX_0b575fa9a4cfe638a925949285; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0b575fa9a4cfe638a925949285" ON public.password_reset_request USING btree (token);


--
-- Name: IDX_0b60ebb3aa0065f10b0616c117; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0b60ebb3aa0065f10b0616c117" ON public.__chart_day__drive USING btree (date);


--
-- Name: IDX_0c44bf4f680964145f2a68a341; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0c44bf4f680964145f2a68a341" ON public.hashtag USING btree ("attachedLocalUsersCount");


--
-- Name: IDX_0d7718e562dcedd0aa5cf2c9f7; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0d7718e562dcedd0aa5cf2c9f7" ON public.user_security_key USING btree ("publicKey");


--
-- Name: IDX_0d801c609cec4e9eb4b6b4490c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0d801c609cec4e9eb4b6b4490c" ON public.renote_muting USING btree ("muterId", "muteeId");


--
-- Name: IDX_0d9a1738f2cf7f3b1c3334dfab; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0d9a1738f2cf7f3b1c3334dfab" ON public.relay USING btree (inbox);


--
-- Name: IDX_0e00498f180193423c992bc437; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0e00498f180193423c992bc437" ON public.note_favorite USING btree ("noteId");


--
-- Name: IDX_0e206cec573f1edff4a3062923; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0e206cec573f1edff4a3062923" ON public.hashtag USING btree ("mentionedLocalUsersCount");


--
-- Name: IDX_0e43068c3f92cab197c3d3cd86; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0e43068c3f92cab197c3d3cd86" ON public.channel_following USING btree ("followeeId");


--
-- Name: IDX_0e61efab7f88dbb79c9166dbb4; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0e61efab7f88dbb79c9166dbb4" ON public.page_like USING btree ("userId");


--
-- Name: IDX_0f4fb9ad355f3effff221ef245; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0f4fb9ad355f3effff221ef245" ON public.note_favorite USING btree ("userId", "noteId");


--
-- Name: IDX_0f58c11241e649d2a638a8de94; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_0f58c11241e649d2a638a8de94" ON public.channel USING btree ("notesCount");


--
-- Name: IDX_0ff69e8dfa9fe31bb4a4660f59; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_0ff69e8dfa9fe31bb4a4660f59" ON public.registration_ticket USING btree (code);


--
-- Name: IDX_1039988afa3bf991185b277fe0; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_1039988afa3bf991185b277fe0" ON public.user_group_invite USING btree ("userId");


--
-- Name: IDX_12c01c0d1a79f77d9f6c15fadd; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_12c01c0d1a79f77d9f6c15fadd" ON public.follow_request USING btree ("followeeId");


--
-- Name: IDX_12c4768a2f706fc267f2078903; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_12c4768a2f706fc267f2078903" ON public.chat_approval USING btree ("userId", "otherId");


--
-- Name: IDX_13565815f618a1ff53886c5b28; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_13565815f618a1ff53886c5b28" ON public.__chart__drive USING btree (date);


--
-- Name: IDX_13761f64257f40c5636d0ff95e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_13761f64257f40c5636d0ff95e" ON public.note_reaction USING btree ("userId");


--
-- Name: IDX_153536c67d05e9adb24e99fc2b; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_153536c67d05e9adb24e99fc2b" ON public.note USING btree (uri);


--
-- Name: IDX_16effb2e888f6763673b579f80; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_16effb2e888f6763673b579f80" ON public.__chart__test_unique USING btree (date) WHERE ("group" IS NULL);


--
-- Name: IDX_171e64971c780ebd23fae140bb; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_171e64971c780ebd23fae140bb" ON public.user_publickey USING btree ("keyId");


--
-- Name: IDX_17cb3553c700a4985dff5a30ff; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_17cb3553c700a4985dff5a30ff" ON public.note USING btree ("replyId");


--
-- Name: IDX_185b6b5afa707b5d36d1ce3144; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_185b6b5afa707b5d36d1ce3144" ON public.chat_room_membership USING btree ("userId", "roomId");


--
-- Name: IDX_1a165c68a49d08f11caffbd206; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_1a165c68a49d08f11caffbd206" ON public.gallery_post USING btree ("likedCount");


--
-- Name: IDX_1a527b423ad0858a1af5a056d4; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_1a527b423ad0858a1af5a056d4" ON public.__chart_day__notes USING btree (date);


--
-- Name: IDX_1eb9d9824a630321a29fd3b290; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_1eb9d9824a630321a29fd3b290" ON public.muting USING btree ("muterId", "muteeId");


--
-- Name: IDX_20e30aa35180e317e133d75316; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_20e30aa35180e317e133d75316" ON public.user_group USING btree ("createdAt");


--
-- Name: IDX_2133ef8317e4bdb839c0dcbf13; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_2133ef8317e4bdb839c0dcbf13" ON public.page USING btree ("userId", name);


--
-- Name: IDX_229a41ad465f9205f1f5703291; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_229a41ad465f9205f1f5703291" ON public.__chart__per_user_reaction USING btree (date, "group");


--
-- Name: IDX_22baca135bb8a3ea1a83d13df3; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_22baca135bb8a3ea1a83d13df3" ON public.registry_item USING btree (scope);


--
-- Name: IDX_24e0042143a18157b234df186c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_24e0042143a18157b234df186c" ON public.following USING btree ("followeeId");


--
-- Name: IDX_25a31662b0b0cc9af6549a9d71; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_25a31662b0b0cc9af6549a9d71" ON public.clip_favorite USING btree ("userId");


--
-- Name: IDX_25a97c02003338124b2b75fdbc; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_25a97c02003338124b2b75fdbc" ON public.__chart__hashtag USING btree (date, "group");


--
-- Name: IDX_25b1dd384bec391b07b74b861c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_25b1dd384bec391b07b74b861c" ON public.note_unread USING btree ("isMentioned");


--
-- Name: IDX_25e097b51d7622c249452c6f75; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_25e097b51d7622c249452c6f75" ON public.chat_message USING btree ("toUserId");


--
-- Name: IDX_26d4ee490b5a487142d35466ee; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_26d4ee490b5a487142d35466ee" ON public.bubble_game_record USING btree (score);


--
-- Name: IDX_2710a55f826ee236ea1a62698f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2710a55f826ee236ea1a62698f" ON public.hashtag USING btree ("mentionedUsersCount");


--
-- Name: IDX_2882b8a1a07c7d281a98b6db16; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_2882b8a1a07c7d281a98b6db16" ON public.promo_read USING btree ("userId", "noteId");


--
-- Name: IDX_29c11c7deb06615076f8c95b80; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_29c11c7deb06615076f8c95b80" ON public.note_thread_muting USING btree ("userId");


--
-- Name: IDX_29e8c1d579af54d4232939f994; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_29e8c1d579af54d4232939f994" ON public.note_unread USING btree ("noteUserId");


--
-- Name: IDX_29ef80c6f13bcea998447fce43; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_29ef80c6f13bcea998447fce43" ON public.channel USING btree ("lastNotedAt");


--
-- Name: IDX_2b15aaf4a0dc5be3499af7ab6a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2b15aaf4a0dc5be3499af7ab6a" ON public.abuse_user_report USING btree (resolved);


--
-- Name: IDX_2b5ec6c574d6802c94c80313fb; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2b5ec6c574d6802c94c80313fb" ON public.clip USING btree ("userId");


--
-- Name: IDX_2c308dbdc50d94dc625670055f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2c308dbdc50d94dc625670055f" ON public.signin USING btree ("userId");


--
-- Name: IDX_2c4be03b446884f9e9c502135b; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2c4be03b446884f9e9c502135b" ON public.messaging_message USING btree ("groupId");


--
-- Name: IDX_2cd4a2743a99671308f5417759; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2cd4a2743a99671308f5417759" ON public.blocking USING btree ("blockeeId");


--
-- Name: IDX_2da24ce20ad209f1d9dc032457; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_2da24ce20ad209f1d9dc032457" ON public.ad USING btree ("expiresAt");


--
-- Name: IDX_2e230dd45a10e671d781d99f3e; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_2e230dd45a10e671d781d99f3e" ON public.channel_following USING btree ("followerId", "followeeId");


--
-- Name: IDX_307be5f1d1252e0388662acb96; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_307be5f1d1252e0388662acb96" ON public.following USING btree ("followerId", "followeeId");


--
-- Name: IDX_30bf67687f483ace115c5ca642; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_30bf67687f483ace115c5ca642" ON public.__chart__per_user_drive USING btree (date, "group");


--
-- Name: IDX_315c779174fe8247ab324f036e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_315c779174fe8247ab324f036e" ON public.drive_file USING btree ("isLink");


--
-- Name: IDX_318cdf42a9cfc11f479bd802bb; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_318cdf42a9cfc11f479bd802bb" ON public.note_watching USING btree ("createdAt");


--
-- Name: IDX_3252a5df8d5bbd16b281f7799e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3252a5df8d5bbd16b281f7799e" ON public."user" USING btree (host);


--
-- Name: IDX_34415e3062ae7a94617496e81c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_34415e3062ae7a94617496e81c" ON public.channel_muting USING btree ("userId");


--
-- Name: IDX_347fec870eafea7b26c8a73bac; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_347fec870eafea7b26c8a73bac" ON public.hashtag USING btree (name);


--
-- Name: IDX_361b500e06721013c124b7b6c5; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_361b500e06721013c124b7b6c5" ON public.user_ip USING btree ("userId", ip);


--
-- Name: IDX_36cb699c49580d4e6c2e6159f9; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_36cb699c49580d4e6c2e6159f9" ON public.__chart__federation USING btree (date);


--
-- Name: IDX_36ef5192a1ce55ed0e40aa4db5; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_36ef5192a1ce55ed0e40aa4db5" ON public.antenna USING btree ("isActive");


--
-- Name: IDX_37bb9a1b4585f8a3beb24c62d6; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_37bb9a1b4585f8a3beb24c62d6" ON public.drive_file USING btree (md5);


--
-- Name: IDX_39ee857ab2f23493037c6b6631; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_39ee857ab2f23493037c6b6631" ON public.__chart__instance USING btree (date, "group");


--
-- Name: IDX_3aa8ea9a8f15214ad91638c0a7; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3aa8ea9a8f15214ad91638c0a7" ON public.flash USING btree ("updatedAt");


--
-- Name: IDX_3b25402709dd9882048c2bbade; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3b25402709dd9882048c2bbade" ON public.reversi_matching USING btree ("parentId");


--
-- Name: IDX_3b33dff77bb64b23c88151d23e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3b33dff77bb64b23c88151d23e" ON public.drive_file USING btree ("maybeSensitive");


--
-- Name: IDX_3befe6f999c86aff06eb0257b4; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3befe6f999c86aff06eb0257b4" ON public.user_profile USING btree ("enableWordMute");


--
-- Name: IDX_3ca50563facd913c425e7a89ee; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3ca50563facd913c425e7a89ee" ON public.gallery_post USING btree ("fileIds");


--
-- Name: IDX_3d6b372788ab01be58853003c9; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3d6b372788ab01be58853003c9" ON public.user_group USING btree ("userId");


--
-- Name: IDX_3ede46f507c87ad698051d56a8; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3ede46f507c87ad698051d56a8" ON public.instance USING btree ("suspensionState");


--
-- Name: IDX_3f5b0899ef90527a3462d7c2cb; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3f5b0899ef90527a3462d7c2cb" ON public.app USING btree ("userId");


--
-- Name: IDX_3fcc2c589eaefc205e0714b99c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_3fcc2c589eaefc205e0714b99c" ON public.ad USING btree ("startsAt");


--
-- Name: IDX_410cd649884b501c02d6e72738; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_410cd649884b501c02d6e72738" ON public.user_note_pining USING btree ("userId", "noteId");


--
-- Name: IDX_41a3c87a37aea616ee459369e1; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_41a3c87a37aea616ee459369e1" ON public.system_account USING btree ("userId");


--
-- Name: IDX_42eb716a37d381cdf566192b2b; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_42eb716a37d381cdf566192b2b" ON public.__chart__notes USING btree (date);


--
-- Name: IDX_44499765eec6b5489d72c4253b; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_44499765eec6b5489d72c4253b" ON public.note_watching USING btree ("noteUserId");


--
-- Name: IDX_45145e4953780f3cd5656f0ea6; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_45145e4953780f3cd5656f0ea6" ON public.note_reaction USING btree ("noteId");


--
-- Name: IDX_47f4b1892f5d6ba8efb3057d81; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_47f4b1892f5d6ba8efb3057d81" ON public.note_favorite USING btree ("userId");


--
-- Name: IDX_48a00f08598662b9ca540521eb; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_48a00f08598662b9ca540521eb" ON public.user_list USING btree ("isPublic");


--
-- Name: IDX_4ae7053179014915d1432d3f40; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_4ae7053179014915d1432d3f40" ON public.bubble_game_record USING btree ("seededAt");


--
-- Name: IDX_4bb7fd4a34492ae0e6cc8d30ac; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_4bb7fd4a34492ae0e6cc8d30ac" ON public.password_reset_request USING btree ("userId");


--
-- Name: IDX_4c02d38a976c3ae132228c6fce; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_4c02d38a976c3ae132228c6fce" ON public.hashtag USING btree ("mentionedRemoteUsersCount");


--
-- Name: IDX_4ccd2239268ebbd1b35e318754; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_4ccd2239268ebbd1b35e318754" ON public.following USING btree ("followerHost");


--
-- Name: IDX_4ce6fb9c70529b4c8ac46c9bfa; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_4ce6fb9c70529b4c8ac46c9bfa" ON public.page_like USING btree ("userId", "pageId");


--
-- Name: IDX_4d534d7177fc59879d942e96d0; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_4d534d7177fc59879d942e96d0" ON public.channel_muting USING btree ("channelId");


--
-- Name: IDX_4e5c4c99175638ec0761714ab0; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_4e5c4c99175638ec0761714ab0" ON public.user_pending USING btree (code);


--
-- Name: IDX_4ebbf7f93cdc10e8d1ef2fc6cd; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_4ebbf7f93cdc10e8d1ef2fc6cd" ON public.abuse_user_report USING btree ("targetUserHost");


--
-- Name: IDX_4f4d35e1256c84ae3d1f0eab10; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_4f4d35e1256c84ae3d1f0eab10" ON public.emoji USING btree (name, host);


--
-- Name: IDX_5048e9daccbbbc6d567bb142d3; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_5048e9daccbbbc6d567bb142d3" ON public.__chart__per_user_notes USING btree (date, "group");


--
-- Name: IDX_50bd7164c5b78f1f4a42c4d21f; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_50bd7164c5b78f1f4a42c4d21f" ON public.poll_vote USING btree ("userId", "noteId", choice);


--
-- Name: IDX_5108098457488634a4768e1d12; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_5108098457488634a4768e1d12" ON public.following USING btree (notify);


--
-- Name: IDX_52ccc804d7c69037d558bac4c9; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_52ccc804d7c69037d558bac4c9" ON public.note USING btree ("renoteId");


--
-- Name: IDX_530257863e1381a7f2f1d3282f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_530257863e1381a7f2f1d3282f" ON public.chat_approval USING btree ("userId");


--
-- Name: IDX_5377c307783fce2b6d352e1203; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_5377c307783fce2b6d352e1203" ON public.messaging_message USING btree ("userId");


--
-- Name: IDX_539b6c08c05067599743bb6389; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_539b6c08c05067599743bb6389" ON public.role_assignment USING btree ("expiresAt");


--
-- Name: IDX_55720b33a61a7c806a8215b825; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_55720b33a61a7c806a8215b825" ON public.drive_file USING btree ("userId", "folderId", id);


--
-- Name: IDX_56b0166d34ddae49d8ef7610bb; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_56b0166d34ddae49d8ef7610bb" ON public.note_unread USING btree ("userId");


--
-- Name: IDX_5900e907bb46516ddf2871327c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_5900e907bb46516ddf2871327c" ON public.emoji USING btree (host);


--
-- Name: IDX_5a056076f76b2efe08216ba655; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_5a056076f76b2efe08216ba655" ON public.webhook USING btree (active);


--
-- Name: IDX_5cc8c468090e129857e9fecce5; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_5cc8c468090e129857e9fecce5" ON public.user_group_invitation USING btree ("userGroupId");


--
-- Name: IDX_5deb01ae162d1d70b80d064c27; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_5deb01ae162d1d70b80d064c27" ON public."user" USING btree ("usernameLower", host);


--
-- Name: IDX_5f265075b215fc390a57523b12; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_5f265075b215fc390a57523b12" ON public.chat_room_invitation USING btree ("roomId");


--
-- Name: IDX_603a7b1e7aa0533c6c88e9bfaf; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_603a7b1e7aa0533c6c88e9bfaf" ON public.announcement_read USING btree ("announcementId");


--
-- Name: IDX_60c4af1c19a7a75f1592f93b28; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_60c4af1c19a7a75f1592f93b28" ON public.flash_like USING btree ("userId");


--
-- Name: IDX_617a8fe225a6e701d89e02d2c7; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_617a8fe225a6e701d89e02d2c7" ON public.__chart_day__federation USING btree (date);


--
-- Name: IDX_62aa5047b5aec92524f24c701d; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_62aa5047b5aec92524f24c701d" ON public.__chart_day__per_user_drive USING btree (date, "group");


--
-- Name: IDX_62cb09e1129f6ec024ef66e183; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_62cb09e1129f6ec024ef66e183" ON public.auth_session USING btree (token);


--
-- Name: IDX_6446c571a0e8d0f05f01c78909; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_6446c571a0e8d0f05f01c78909" ON public.antenna USING btree ("userId");


--
-- Name: IDX_64c327441248bae40f7d92f34f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_64c327441248bae40f7d92f34f" ON public.access_token USING btree (hash);


--
-- Name: IDX_650b49c5639b5840ee6a2b8f83; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_650b49c5639b5840ee6a2b8f83" ON public.user_memo USING btree ("userId");


--
-- Name: IDX_6516c5a6f3c015b4eed39978be; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_6516c5a6f3c015b4eed39978be" ON public.following USING btree ("followerId");


--
-- Name: IDX_66ac4a82894297fd09ba61f3d3; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_66ac4a82894297fd09ba61f3d3" ON public.user_memo USING btree ("targetUserId");


--
-- Name: IDX_66d2bd2ee31d14bcc23069a89f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_66d2bd2ee31d14bcc23069a89f" ON public.poll_vote USING btree ("userId");


--
-- Name: IDX_67dc758bc0566985d1b3d39986; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_67dc758bc0566985d1b3d39986" ON public.user_group_joining USING btree ("userGroupId");


--
-- Name: IDX_68881008f7c3588ad7ecae471c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_68881008f7c3588ad7ecae471c" ON public.user_note_pining USING btree ("noteId");


--
-- Name: IDX_6a57f051d82c6d4036c141e107; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_6a57f051d82c6d4036c141e107" ON public.note_unread USING btree ("noteChannelId");


--
-- Name: IDX_6d8084ec9496e7334a4602707e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_6d8084ec9496e7334a4602707e" ON public.channel_following USING btree ("followerId");


--
-- Name: IDX_6dd314e96806b7df65ddadff72; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_6dd314e96806b7df65ddadff72" ON public.channel_muting USING btree ("expiresAt");


--
-- Name: IDX_6fc0ec357d55a18646262fdfff; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_6fc0ec357d55a18646262fdfff" ON public.clip_note USING btree ("noteId", "clipId");


--
-- Name: IDX_70ba8f6af34bc924fc9e12adb8; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_70ba8f6af34bc924fc9e12adb8" ON public.access_token USING btree (token);


--
-- Name: IDX_7125a826ab192eb27e11d358a5; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_7125a826ab192eb27e11d358a5" ON public.note USING btree ("userHost");


--
-- Name: IDX_75276757070d21fdfaf4c05290; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_75276757070d21fdfaf4c05290" ON public.bubble_game_record USING btree ("userId");


--
-- Name: IDX_78787741f9010886796f2320a4; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_78787741f9010886796f2320a4" ON public.user_group_invite USING btree ("userId", "userGroupId");


--
-- Name: IDX_79a26e7a4d9afa5e4fc05f134e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_79a26e7a4d9afa5e4fc05f134e" ON public.chat_message USING btree ("fromUserId");


--
-- Name: IDX_7aa72a5fe76019bfe8e5e0e8b7; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_7aa72a5fe76019bfe8e5e0e8b7" ON public.renote_muting USING btree ("muterId");


--
-- Name: IDX_7b8d9225168e962f94ea517e00; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_7b8d9225168e962f94ea517e00" ON public.announcement USING btree (silence);


--
-- Name: IDX_7eac97594bcac5ffcf2068089b; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_7eac97594bcac5ffcf2068089b" ON public.renote_muting USING btree ("muteeId");


--
-- Name: IDX_7f7f1c66f48e9a8e18a33bc515; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_7f7f1c66f48e9a8e18a33bc515" ON public.user_ip USING btree ("userId");


--
-- Name: IDX_7fa20a12319c7f6dc3aed98c0a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_7fa20a12319c7f6dc3aed98c0a" ON public.poll USING btree ("userHost");


--
-- Name: IDX_8063a0586ed1dfbe86e982d961; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8063a0586ed1dfbe86e982d961" ON public.webhook USING btree ("on");


--
-- Name: IDX_80ca6e6ef65fb9ef34ea8c90f4; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_80ca6e6ef65fb9ef34ea8c90f4" ON public."user" USING btree ("updatedAt");


--
-- Name: IDX_8125f950afd3093acb10d2db8a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8125f950afd3093acb10d2db8a" ON public.channel_note_pining USING btree ("channelId");


--
-- Name: IDX_823bae55bd81b3be6e05cff438; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_823bae55bd81b3be6e05cff438" ON public.channel USING btree ("userId");


--
-- Name: IDX_8288151386172b8109f7239ab2; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8288151386172b8109f7239ab2" ON public.announcement_read USING btree ("userId");


--
-- Name: IDX_8302bd27226605ece14842fb25; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8302bd27226605ece14842fb25" ON public.channel_favorite USING btree ("userId");


--
-- Name: IDX_83f0862e9bae44af52ced7099e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_83f0862e9bae44af52ced7099e" ON public.promo_note USING btree ("userId");


--
-- Name: IDX_845254b3eaf708ae8a6cac3026; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_845254b3eaf708ae8a6cac3026" ON public.__chart__users USING btree (date);


--
-- Name: IDX_8552bb38e7ed038c5bdd398a38; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8552bb38e7ed038c5bdd398a38" ON public.chat_room_invitation USING btree ("userId");


--
-- Name: IDX_860fa6f6c7df5bb887249fba22; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_860fa6f6c7df5bb887249fba22" ON public.drive_file USING btree ("userId");


--
-- Name: IDX_89a29c9237b8c3b6b3cbb4cb30; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_89a29c9237b8c3b6b3cbb4cb30" ON public.note_unread USING btree ("isSpecified");


--
-- Name: IDX_8bdcd3dd2bddb78014999a16ce; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8bdcd3dd2bddb78014999a16ce" ON public.drive_file USING btree ("maybePorn");


--
-- Name: IDX_8bfa548c2b31f9e07db113773e; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_8bfa548c2b31f9e07db113773e" ON public.__chart_day__network USING btree (date);


--
-- Name: IDX_8d5afc98982185799b160e10eb; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_8d5afc98982185799b160e10eb" ON public.instance USING btree (host);


--
-- Name: IDX_8f589cf056ff51f09d6096f645; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_8f589cf056ff51f09d6096f645" ON public.__chart_day__hashtag USING btree (date, "group");


--
-- Name: IDX_8fd5215095473061855ceb948c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_8fd5215095473061855ceb948c" ON public.gallery_like USING btree ("userId");


--
-- Name: IDX_90148bbc2bf0854428786bfc15; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_90148bbc2bf0854428786bfc15" ON public.page USING btree ("visibleUserIds");


--
-- Name: IDX_924fa71815cfa3941d003702a0; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_924fa71815cfa3941d003702a0" ON public.announcement_read USING btree ("userId", "announcementId");


--
-- Name: IDX_92779627994ac79277f070c91e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_92779627994ac79277f070c91e" ON public.drive_file USING btree ("userHost");


--
-- Name: IDX_93060675b4a79a577f31d260c6; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_93060675b4a79a577f31d260c6" ON public.muting USING btree ("muterId");


--
-- Name: IDX_9657d55550c3d37bfafaf7d4b0; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_9657d55550c3d37bfafaf7d4b0" ON public.promo_read USING btree ("userId");


--
-- Name: IDX_97754ca6f2baff9b4abb7f853d; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_97754ca6f2baff9b4abb7f853d" ON public.sw_subscription USING btree ("userId");


--
-- Name: IDX_985b836dddd8615e432d7043dd; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_985b836dddd8615e432d7043dd" ON public.gallery_post USING btree ("userId");


--
-- Name: IDX_98a1bc5cb30dfd159de056549f; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_98a1bc5cb30dfd159de056549f" ON public.blocking USING btree ("blockerId", "blockeeId");


--
-- Name: IDX_9949557d0e1b2c19e5344c171e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_9949557d0e1b2c19e5344c171e" ON public.access_token USING btree ("userId");


--
-- Name: IDX_9b88250fc2fd009b8f1b5623ed; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_9b88250fc2fd009b8f1b5623ed" ON public.flash USING btree ("userId");


--
-- Name: IDX_EMOJI_CATEGORY; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_EMOJI_CATEGORY" ON public.emoji USING btree (category);


--
-- Name: IDX_EMOJI_ROLE_IDS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_EMOJI_ROLE_IDS" ON public.emoji USING gin ("roleIdsThatCanBeUsedThisEmojiAsReaction");


--
-- Name: IDX_NOTE_DRAFT_CHANNEL_ID; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_DRAFT_CHANNEL_ID" ON public.note_draft USING btree ("channelId");


--
-- Name: IDX_NOTE_DRAFT_FILE_IDS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_DRAFT_FILE_IDS" ON public.note_draft USING gin ("fileIds");


--
-- Name: IDX_NOTE_DRAFT_RENOTE_ID; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_DRAFT_RENOTE_ID" ON public.note_draft USING btree ("renoteId");


--
-- Name: IDX_NOTE_DRAFT_REPLY_ID; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_DRAFT_REPLY_ID" ON public.note_draft USING btree ("replyId");


--
-- Name: IDX_NOTE_DRAFT_USER_ID; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_DRAFT_USER_ID" ON public.note_draft USING btree ("userId");


--
-- Name: IDX_NOTE_DRAFT_VISIBLE_USER_IDS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_DRAFT_VISIBLE_USER_IDS" ON public.note_draft USING gin ("visibleUserIds");


--
-- Name: IDX_NOTE_FILE_IDS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_FILE_IDS" ON public.note USING gin ("fileIds");


--
-- Name: IDX_NOTE_MENTIONS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_MENTIONS" ON public.note USING gin (mentions);


--
-- Name: IDX_NOTE_TAGS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_TAGS" ON public.note USING gin (tags);


--
-- Name: IDX_NOTE_VISIBLE_USER_IDS; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_NOTE_VISIBLE_USER_IDS" ON public.note USING gin ("visibleUserIds");


--
-- Name: IDX_USERPROFILE_BIRTHDAY_DATE; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_USERPROFILE_BIRTHDAY_DATE" ON public.user_profile USING btree (public.get_birthday_date((birthday)::text));


--
-- Name: IDX_a012eaf5c87c65da1deb5fdbfa; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a012eaf5c87c65da1deb5fdbfa" ON public.clip_note USING btree ("noteId");


--
-- Name: IDX_a08ad074601d204e0f69da9a95; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a08ad074601d204e0f69da9a95" ON public.moderation_log USING btree ("userId");


--
-- Name: IDX_a0cd75442dd10d0643a17c4a49; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_a0cd75442dd10d0643a17c4a49" ON public.__chart__test_unique USING btree (date, "group");


--
-- Name: IDX_a1efd3e0048a5f2793a47360dc; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_a1efd3e0048a5f2793a47360dc" ON public.__chart__network USING btree (date);


--
-- Name: IDX_a27b942a0d6dcff90e3ee9b5e8; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a27b942a0d6dcff90e3ee9b5e8" ON public."user" USING btree ("usernameLower");


--
-- Name: IDX_a319e5dbf47e8a17497623beae; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_a319e5dbf47e8a17497623beae" ON public.__chart__test USING btree (date, "group");


--
-- Name: IDX_a3eac04ae2aa9e221e7596114a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a3eac04ae2aa9e221e7596114a" ON public.clip USING btree ("lastClippedAt");


--
-- Name: IDX_a40b8df8c989d7db937ea27cf6; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a40b8df8c989d7db937ea27cf6" ON public.drive_file USING btree (type);


--
-- Name: IDX_a42c93c69989ce1d09959df4cf; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_a42c93c69989ce1d09959df4cf" ON public.note_watching USING btree ("userId", "noteId");


--
-- Name: IDX_a6f649630f55af3888e5a42919; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a6f649630f55af3888e5a42919" ON public.note USING btree ("userId", id DESC);


--
-- Name: IDX_a7eba67f8b3fa27271e85d2e26; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a7eba67f8b3fa27271e85d2e26" ON public.drive_file USING btree ("isSensitive");


--
-- Name: IDX_a7fd92dd6dc519e6fb435dd108; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a7fd92dd6dc519e6fb435dd108" ON public.follow_request USING btree ("followerId");


--
-- Name: IDX_a848f66d6cec11980a5dd59582; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_a848f66d6cec11980a5dd59582" ON public.__chart_day__ap_request USING btree (date);


--
-- Name: IDX_a854e557b1b14814750c7c7b0c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_a854e557b1b14814750c7c7b0c" ON public."user" USING btree (token);


--
-- Name: IDX_a9021cc2e1feb5f72d3db6e9f5; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_a9021cc2e1feb5f72d3db6e9f5" ON public.abuse_user_report USING btree ("targetUserId");


--
-- Name: IDX_abuse_report_notification_recipient_isActive; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_abuse_report_notification_recipient_isActive" ON public.abuse_report_notification_recipient USING btree ("isActive");


--
-- Name: IDX_abuse_report_notification_recipient_method; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_abuse_report_notification_recipient_method" ON public.abuse_report_notification_recipient USING btree (method);


--
-- Name: IDX_abuse_report_notification_recipient_systemWebhookId; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_abuse_report_notification_recipient_systemWebhookId" ON public.abuse_report_notification_recipient USING btree ("systemWebhookId");


--
-- Name: IDX_abuse_report_notification_recipient_userId; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_abuse_report_notification_recipient_userId" ON public.abuse_report_notification_recipient USING btree ("userId");


--
-- Name: IDX_ad0c221b25672daf2df320a817; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_ad0c221b25672daf2df320a817" ON public.note_reaction USING btree ("userId", "noteId");


--
-- Name: IDX_ae1d917992dd0c9d9bbdad06c4; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_ae1d917992dd0c9d9bbdad06c4" ON public.page USING btree ("userId");


--
-- Name: IDX_ae7aab18a2641d3e5f25e0c4ea; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_ae7aab18a2641d3e5f25e0c4ea" ON public.note_thread_muting USING btree ("userId", "threadId");


--
-- Name: IDX_aecfbd5ef60374918e63ee95fa; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_aecfbd5ef60374918e63ee95fa" ON public.poll_vote USING btree ("noteId");


--
-- Name: IDX_af639b066dfbca78b01a920f8a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_af639b066dfbca78b01a920f8a" ON public.page USING btree ("updatedAt");


--
-- Name: IDX_b0134ec406e8d09a540f818288; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b0134ec406e8d09a540f818288" ON public.note_watching USING btree ("userId");


--
-- Name: IDX_b14489029e4b3aaf4bba5fb524; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_b14489029e4b3aaf4bba5fb524" ON public.__chart__test_grouped USING btree (date, "group");


--
-- Name: IDX_b1754a39d0b281e07ed7c078ec; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_b1754a39d0b281e07ed7c078ec" ON public.clip_favorite USING btree ("userId", "clipId");


--
-- Name: IDX_b1d46037f23d170da5c05fdf75; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b1d46037f23d170da5c05fdf75" ON public.chat_approval USING btree ("otherId");


--
-- Name: IDX_b37dafc86e9af007e3295c2781; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b37dafc86e9af007e3295c2781" ON public.emoji USING btree (name);


--
-- Name: IDX_b6f93f2f30bdbb9a5ebdc7c718; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b6f93f2f30bdbb9a5ebdc7c718" ON public.registration_ticket USING btree ("usedById");


--
-- Name: IDX_b77d4dd9562c3a899d9a286fcd; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_b77d4dd9562c3a899d9a286fcd" ON public.__chart__per_user_following USING btree (date, "group");


--
-- Name: IDX_b7fcefbdd1c18dce86687531f9; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b7fcefbdd1c18dce86687531f9" ON public.user_list USING btree ("userId");


--
-- Name: IDX_b82c19c08afb292de4600d99e4; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b82c19c08afb292de4600d99e4" ON public.page USING btree (name);


--
-- Name: IDX_b96870ed326ccc7fa243970965; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_b96870ed326ccc7fa243970965" ON public.channel_muting USING btree ("userId", "channelId");


--
-- Name: IDX_bb90d1956dafc4068c28aa7560; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_bb90d1956dafc4068c28aa7560" ON public.drive_file USING btree ("folderId");


--
-- Name: IDX_bc1afcc8ef7e9400cdc3c0a87e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_bc1afcc8ef7e9400cdc3c0a87e" ON public.announcement USING btree ("isActive");


--
-- Name: IDX_be623adaa4c566baf5d29ce0c8; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_be623adaa4c566baf5d29ce0c8" ON public."user" USING btree (uri);


--
-- Name: IDX_beba993576db0261a15364ea96; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_beba993576db0261a15364ea96" ON public.registration_ticket USING btree ("createdById");


--
-- Name: IDX_bf3a053c07d9fb5d87317c56ee; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_bf3a053c07d9fb5d87317c56ee" ON public.access_token USING btree (session);


--
-- Name: IDX_bfbc6305547539369fe73eb144; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_bfbc6305547539369fe73eb144" ON public.user_group_invitation USING btree ("userId");


--
-- Name: IDX_bfbc6f79ba4007b4ce5097f08d; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_bfbc6f79ba4007b4ce5097f08d" ON public.user_note_pining USING btree ("userId");


--
-- Name: IDX_c1240fcc9675946ea5d6c2860e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_c1240fcc9675946ea5d6c2860e" ON public.poll USING btree ("channelId");


--
-- Name: IDX_c1fd1c3dfb0627aa36c253fd14; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_c1fd1c3dfb0627aa36c253fd14" ON public.muting USING btree ("expiresAt");


--
-- Name: IDX_c25143ebab714e930aeca1c0e8; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_c25143ebab714e930aeca1c0e8" ON public.chat_room_membership USING btree ("roomId");


--
-- Name: IDX_c362033aee0ea51011386a5a7e; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_c362033aee0ea51011386a5a7e" ON public.system_account USING btree (type);


--
-- Name: IDX_c426394644267453e76f036926; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_c426394644267453e76f036926" ON public.note_thread_muting USING btree ("threadId");


--
-- Name: IDX_c5545d4b31cdc684034e33b81c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_c5545d4b31cdc684034e33b81c" ON public.__chart_day__per_user_notes USING btree (date, "group");


--
-- Name: IDX_c55b2b7c284d9fef98026fc88e; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_c55b2b7c284d9fef98026fc88e" ON public.drive_file USING btree ("webpublicAccessKey");


--
-- Name: IDX_c71faf11f0a28a5c0bb506203c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_c71faf11f0a28a5c0bb506203c" ON public.channel_favorite USING btree ("userId", "channelId");


--
-- Name: IDX_c8cc87bd0f2f4487d17c651fbf; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_c8cc87bd0f2f4487d17c651fbf" ON public."user" USING btree ("lastActiveDate");


--
-- Name: IDX_cac14a4e3944454a5ce7daa514; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_cac14a4e3944454a5ce7daa514" ON public.messaging_message USING btree ("recipientId");


--
-- Name: IDX_cad6e07c20037f31cdba8a350c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_cad6e07c20037f31cdba8a350c" ON public.__chart_day__users USING btree (date);


--
-- Name: IDX_cc7c72974f1b2f385a8921f094; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_cc7c72974f1b2f385a8921f094" ON public.channel USING btree ("isArchived");


--
-- Name: IDX_cddcaf418dc4d392ecfcca842a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_cddcaf418dc4d392ecfcca842a" ON public.user_list_membership USING btree ("userListId");


--
-- Name: IDX_ce62b50d882d4e9dee10ad0d2f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_ce62b50d882d4e9dee10ad0d2f" ON public.following USING btree ("followeeId", "followerHost", "isFollowerHibernated");


--
-- Name: IDX_cfbfeeccb0cbedcd660b17eb07; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_cfbfeeccb0cbedcd660b17eb07" ON public.flash_like USING btree ("userId", "flashId");


--
-- Name: IDX_d3ca0db011b75ac2a940a2337d; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_d3ca0db011b75ac2a940a2337d" ON public.channel_favorite USING btree ("channelId");


--
-- Name: IDX_d4ebdef929896d6dc4a3c5bb48; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_d4ebdef929896d6dc4a3c5bb48" ON public.note USING btree ("threadId");


--
-- Name: IDX_d54a512b822fac7ed52800f6b4; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d54a512b822fac7ed52800f6b4" ON public.follow_request USING btree ("followerId", "followeeId");


--
-- Name: IDX_d54b653660d808b118e36c184c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d54b653660d808b118e36c184c" ON public.__chart_day__per_user_reaction USING btree (date, "group");


--
-- Name: IDX_d57f9030cd3af7f63ffb1c267c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_d57f9030cd3af7f63ffb1c267c" ON public.hashtag USING btree ("attachedUsersCount");


--
-- Name: IDX_d5954f3df5e5e3bdfc3c03f390; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d5954f3df5e5e3bdfc3c03f390" ON public.__chart_day__active_users USING btree (date);


--
-- Name: IDX_d5a1b83c7cab66f167e6888188; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_d5a1b83c7cab66f167e6888188" ON public."user" USING btree ("isExplorable");


--
-- Name: IDX_d6765a8c2a4c17c33f9d7f948b; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d6765a8c2a4c17c33f9d7f948b" ON public.user_list_favorite USING btree ("userId", "userListId");


--
-- Name: IDX_d85a184c2540d2deba33daf642; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d85a184c2540d2deba33daf642" ON public.drive_file USING btree ("accessKey");


--
-- Name: IDX_d908433a4953cc13216cd9c274; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d908433a4953cc13216cd9c274" ON public.note_unread USING btree ("userId", "noteId");


--
-- Name: IDX_d99c5279460fb77ef58c596ce5; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_d99c5279460fb77ef58c596ce5" ON public.chat_room_membership USING btree ("userId");


--
-- Name: IDX_d9ecaed8c6dc43f3592c229282; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_d9ecaed8c6dc43f3592c229282" ON public.user_group_joining USING btree ("userId", "userGroupId");


--
-- Name: IDX_da522b4008a9f5d7743b87ad55; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_da522b4008a9f5d7743b87ad55" ON public.__chart__test_grouped USING btree (date) WHERE ("group" IS NULL);


--
-- Name: IDX_da795d3a83187e8832005ba19d; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_da795d3a83187e8832005ba19d" ON public.announcement USING btree ("forExistingUsers");


--
-- Name: IDX_dab383a36f3c9db4a0c9b02cf3; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_dab383a36f3c9db4a0c9b02cf3" ON public.__chart__test USING btree (date) WHERE ("group" IS NULL);


--
-- Name: IDX_db5b72c16227c97ca88734d5c2; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_db5b72c16227c97ca88734d5c2" ON public.role_assignment USING btree ("userId");


--
-- Name: IDX_dce530b98e454793dac5ec2f5a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_dce530b98e454793dac5ec2f5a" ON public.user_profile USING btree ("userHost");


--
-- Name: IDX_df1b5f4099e99fb0bc5eae53b6; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_df1b5f4099e99fb0bc5eae53b6" ON public.gallery_like USING btree ("userId", "postId");


--
-- Name: IDX_e10924607d058004304611a436; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_e10924607d058004304611a436" ON public.user_group_invite USING btree ("userGroupId");


--
-- Name: IDX_e21cd3646e52ef9c94aaf17c2e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_e21cd3646e52ef9c94aaf17c2e" ON public.messaging_message USING btree ("createdAt");


--
-- Name: IDX_e247b23a3c9b45f89ec1299d06; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_e247b23a3c9b45f89ec1299d06" ON public.reversi_matching USING btree ("childId");


--
-- Name: IDX_e4849a3231f38281280ea4c0ee; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_e4849a3231f38281280ea4c0ee" ON public.__chart_day__per_user_following USING btree (date, "group");


--
-- Name: IDX_e4f3094c43f2d665e6030b0337; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_e4f3094c43f2d665e6030b0337" ON public.user_list_membership USING btree ("userId", "userListId");


--
-- Name: IDX_e56f4beac5746d44bc3e19c80d; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_e56f4beac5746d44bc3e19c80d" ON public.__chart__ap_request USING btree (date);


--
-- Name: IDX_e5848eac4940934e23dbc17581; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_e5848eac4940934e23dbc17581" ON public.drive_file USING btree (uri);


--
-- Name: IDX_e637cba4dc4410218c4251260e; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_e637cba4dc4410218c4251260e" ON public.note_unread USING btree ("noteId");


--
-- Name: IDX_e74022ce9a074b3866f70e0d27; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_e74022ce9a074b3866f70e0d27" ON public.drive_file USING btree ("thumbnailAccessKey");


--
-- Name: IDX_e9793f65f504e5a31fbaedbf2f; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_e9793f65f504e5a31fbaedbf2f" ON public.user_group_invitation USING btree ("userId", "userGroupId");


--
-- Name: IDX_ebe99317bbbe9968a0c6f579ad; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_ebe99317bbbe9968a0c6f579ad" ON public.clip_note USING btree ("clipId");


--
-- Name: IDX_ec96b4fed9dae517e0dbbe0675; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_ec96b4fed9dae517e0dbbe0675" ON public.muting USING btree ("muteeId");


--
-- Name: IDX_f006b8a76efd1abf9f221c175c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f006b8a76efd1abf9f221c175c" ON public.chat_message USING btree ("toRoomId");


--
-- Name: IDX_f0d8ad64243fa2ca2800da0dfd; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f0d8ad64243fa2ca2800da0dfd" ON public.chat_room USING btree ("ownerId");


--
-- Name: IDX_f0de67fd09cd3cd0aabca79994; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f0de67fd09cd3cd0aabca79994" ON public.role_assignment USING btree ("roleId");


--
-- Name: IDX_f22169eb10657bded6d875ac8f; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f22169eb10657bded6d875ac8f" ON public.note USING btree ("channelId");


--
-- Name: IDX_f221e45cfac5bea0ce0f3149fb; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_f221e45cfac5bea0ce0f3149fb" ON public.__chart_day__per_user_pv USING btree (date, "group");


--
-- Name: IDX_f272c8c8805969e6a6449c77b3; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f272c8c8805969e6a6449c77b3" ON public.webhook USING btree ("userId");


--
-- Name: IDX_f2a56da57921ca8439f45c1d95; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_f2a56da57921ca8439f45c1d95" ON public.__chart__per_user_pv USING btree (date, "group");


--
-- Name: IDX_f2d744d9a14d0dfb8b96cb7fc5; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f2d744d9a14d0dfb8b96cb7fc5" ON public.gallery_post USING btree ("isSensitive");


--
-- Name: IDX_f36fed37d6d4cdcc68c803cd9c; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_f36fed37d6d4cdcc68c803cd9c" ON public.channel_note_pining USING btree ("channelId", "noteId");


--
-- Name: IDX_f3a1b4bd0c7cabba958a0c0b23; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f3a1b4bd0c7cabba958a0c0b23" ON public.user_group_joining USING btree ("userId");


--
-- Name: IDX_f49922d511d666848f250663c4; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f49922d511d666848f250663c4" ON public.app USING btree (secret);


--
-- Name: IDX_f4fc06e49c0171c85f1c48060d; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f4fc06e49c0171c85f1c48060d" ON public.drive_folder USING btree ("userId");


--
-- Name: IDX_f631d37835adb04792e361807c; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f631d37835adb04792e361807c" ON public.gallery_post USING btree ("updatedAt");


--
-- Name: IDX_f7b9d338207e40e768e4a5265a; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f7b9d338207e40e768e4a5265a" ON public.instance USING btree ("firstRetrievedAt");


--
-- Name: IDX_f7c3576b37bd2eec966ae24477; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_f7c3576b37bd2eec966ae24477" ON public.retention_aggregation USING btree ("dateKey");


--
-- Name: IDX_f8d8b93740ad12c4ce8213a199; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_f8d8b93740ad12c4ce8213a199" ON public.abuse_user_report USING btree ("reporterHost");


--
-- Name: IDX_fa99d777623947a5b05f394cae; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_fa99d777623947a5b05f394cae" ON public."user" USING btree (tags);


--
-- Name: IDX_faef300913c738265638ba3ebc; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_faef300913c738265638ba3ebc" ON public.user_memo USING btree ("userId", "targetUserId");


--
-- Name: IDX_fb9d21ba0abb83223263df6bcb; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_fb9d21ba0abb83223263df6bcb" ON public.registry_item USING btree ("userId");


--
-- Name: IDX_fcdafee716dfe9c3b5fde90f30; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_fcdafee716dfe9c3b5fde90f30" ON public.following USING btree ("followeeHost");


--
-- Name: IDX_fd25dfe3da37df1715f11ba6ec; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_fd25dfe3da37df1715f11ba6ec" ON public.announcement USING btree ("userId");


--
-- Name: IDX_fea7c0278325a1a2492f2d6acb; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX "IDX_fea7c0278325a1a2492f2d6acb" ON public.__chart_day__instance USING btree (date, "group");


--
-- Name: IDX_ff9ca3b5f3ee3d0681367a9b44; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_ff9ca3b5f3ee3d0681367a9b44" ON public.user_security_key USING btree ("userId");


--
-- Name: IDX_system_webhook_isActive; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_system_webhook_isActive" ON public.system_webhook USING btree ("isActive");


--
-- Name: IDX_system_webhook_on; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX "IDX_system_webhook_on" ON public.system_webhook USING gin ("on");


--
-- Name: drive_folder FK_00ceffb0cdc238b3233294f08f2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.drive_folder
    ADD CONSTRAINT "FK_00ceffb0cdc238b3233294f08f2" FOREIGN KEY ("parentId") REFERENCES public.drive_folder(id) ON DELETE SET NULL;


--
-- Name: user_list_favorite FK_016f613dc4feb807e03e3e7da92; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list_favorite
    ADD CONSTRAINT "FK_016f613dc4feb807e03e3e7da92" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_list_membership FK_021015e6683570ae9f6b0c62bee; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list_membership
    ADD CONSTRAINT "FK_021015e6683570ae9f6b0c62bee" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: note_watching FK_03e7028ab8388a3f5e3ce2a8619; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_watching
    ADD CONSTRAINT "FK_03e7028ab8388a3f5e3ce2a8619" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: abuse_user_report FK_04cc96756f89d0b7f9473e8cdf3; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_user_report
    ADD CONSTRAINT "FK_04cc96756f89d0b7f9473e8cdf3" FOREIGN KEY ("reporterId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: blocking FK_0627125f1a8a42c9a1929edb552; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.blocking
    ADD CONSTRAINT "FK_0627125f1a8a42c9a1929edb552" FOREIGN KEY ("blockerId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: abuse_user_report FK_08b883dd5fdd6f9c4c1572b36de; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_user_report
    ADD CONSTRAINT "FK_08b883dd5fdd6f9c4c1572b36de" FOREIGN KEY ("assigneeId") REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: note_favorite FK_0e00498f180193423c992bc4370; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_favorite
    ADD CONSTRAINT "FK_0e00498f180193423c992bc4370" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: channel_following FK_0e43068c3f92cab197c3d3cd86e; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_following
    ADD CONSTRAINT "FK_0e43068c3f92cab197c3d3cd86e" FOREIGN KEY ("followeeId") REFERENCES public.channel(id) ON DELETE CASCADE;


--
-- Name: page_like FK_0e61efab7f88dbb79c9166dbb48; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.page_like
    ADD CONSTRAINT "FK_0e61efab7f88dbb79c9166dbb48" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_group_invite FK_1039988afa3bf991185b277fe03; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_invite
    ADD CONSTRAINT "FK_1039988afa3bf991185b277fe03" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: channel_note_pining FK_10b19ef67d297ea9de325cd4502; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_note_pining
    ADD CONSTRAINT "FK_10b19ef67d297ea9de325cd4502" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: user_publickey FK_10c146e4b39b443ede016f6736d; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_publickey
    ADD CONSTRAINT "FK_10c146e4b39b443ede016f6736d" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: follow_request FK_12c01c0d1a79f77d9f6c15fadd2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.follow_request
    ADD CONSTRAINT "FK_12c01c0d1a79f77d9f6c15fadd2" FOREIGN KEY ("followeeId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: note_reaction FK_13761f64257f40c5636d0ff95ee; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_reaction
    ADD CONSTRAINT "FK_13761f64257f40c5636d0ff95ee" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: following FK_24e0042143a18157b234df186c3; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.following
    ADD CONSTRAINT "FK_24e0042143a18157b234df186c3" FOREIGN KEY ("followeeId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: clip_favorite FK_25a31662b0b0cc9af6549a9d711; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip_favorite
    ADD CONSTRAINT "FK_25a31662b0b0cc9af6549a9d711" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: chat_message FK_25e097b51d7622c249452c6f757; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message
    ADD CONSTRAINT "FK_25e097b51d7622c249452c6f757" FOREIGN KEY ("toUserId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: note_thread_muting FK_29c11c7deb06615076f8c95b80a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_thread_muting
    ADD CONSTRAINT "FK_29c11c7deb06615076f8c95b80a" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: clip FK_2b5ec6c574d6802c94c80313fb2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip
    ADD CONSTRAINT "FK_2b5ec6c574d6802c94c80313fb2" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: signin FK_2c308dbdc50d94dc625670055f7; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.signin
    ADD CONSTRAINT "FK_2c308dbdc50d94dc625670055f7" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: messaging_message FK_2c4be03b446884f9e9c502135be; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.messaging_message
    ADD CONSTRAINT "FK_2c4be03b446884f9e9c502135be" FOREIGN KEY ("groupId") REFERENCES public.user_group(id) ON DELETE CASCADE;


--
-- Name: blocking FK_2cd4a2743a99671308f5417759e; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.blocking
    ADD CONSTRAINT "FK_2cd4a2743a99671308f5417759e" FOREIGN KEY ("blockeeId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: channel_muting FK_34415e3062ae7a94617496e81c5; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_muting
    ADD CONSTRAINT "FK_34415e3062ae7a94617496e81c5" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: reversi_matching FK_3b25402709dd9882048c2bbade0; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reversi_matching
    ADD CONSTRAINT "FK_3b25402709dd9882048c2bbade0" FOREIGN KEY ("parentId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_group FK_3d6b372788ab01be58853003c93; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group
    ADD CONSTRAINT "FK_3d6b372788ab01be58853003c93" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: app FK_3f5b0899ef90527a3462d7c2cb3; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.app
    ADD CONSTRAINT "FK_3f5b0899ef90527a3462d7c2cb3" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: system_account FK_41a3c87a37aea616ee459369e12; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.system_account
    ADD CONSTRAINT "FK_41a3c87a37aea616ee459369e12" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: note_reaction FK_45145e4953780f3cd5656f0ea6a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_reaction
    ADD CONSTRAINT "FK_45145e4953780f3cd5656f0ea6a" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: note_favorite FK_47f4b1892f5d6ba8efb3057d81a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_favorite
    ADD CONSTRAINT "FK_47f4b1892f5d6ba8efb3057d81a" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: password_reset_request FK_4bb7fd4a34492ae0e6cc8d30ac8; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.password_reset_request
    ADD CONSTRAINT "FK_4bb7fd4a34492ae0e6cc8d30ac8" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_list_favorite FK_4d52b20bfe32c8552e7a61e80d2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list_favorite
    ADD CONSTRAINT "FK_4d52b20bfe32c8552e7a61e80d2" FOREIGN KEY ("userListId") REFERENCES public.user_list(id) ON DELETE CASCADE;


--
-- Name: channel_muting FK_4d534d7177fc59879d942e96d03; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_muting
    ADD CONSTRAINT "FK_4d534d7177fc59879d942e96d03" FOREIGN KEY ("channelId") REFERENCES public.channel(id) ON DELETE CASCADE;


--
-- Name: user_profile FK_51cb79b5555effaf7d69ba1cff9; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_profile
    ADD CONSTRAINT "FK_51cb79b5555effaf7d69ba1cff9" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: chat_approval FK_530257863e1381a7f2f1d3282fe; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_approval
    ADD CONSTRAINT "FK_530257863e1381a7f2f1d3282fe" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: messaging_message FK_535def119223ac05ad3fa9ef64b; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.messaging_message
    ADD CONSTRAINT "FK_535def119223ac05ad3fa9ef64b" FOREIGN KEY ("fileId") REFERENCES public.drive_file(id) ON DELETE CASCADE;


--
-- Name: messaging_message FK_5377c307783fce2b6d352e1203b; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.messaging_message
    ADD CONSTRAINT "FK_5377c307783fce2b6d352e1203b" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: note_unread FK_56b0166d34ddae49d8ef7610bb9; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_unread
    ADD CONSTRAINT "FK_56b0166d34ddae49d8ef7610bb9" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user FK_58f5c71eaab331645112cf8cfa5; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT "FK_58f5c71eaab331645112cf8cfa5" FOREIGN KEY ("avatarId") REFERENCES public.drive_file(id) ON DELETE SET NULL;


--
-- Name: note FK_5b87d9d19127bd5d92026017a7b; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note
    ADD CONSTRAINT "FK_5b87d9d19127bd5d92026017a7b" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_group_invitation FK_5cc8c468090e129857e9fecce5a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_invitation
    ADD CONSTRAINT "FK_5cc8c468090e129857e9fecce5a" FOREIGN KEY ("userGroupId") REFERENCES public.user_group(id) ON DELETE CASCADE;


--
-- Name: chat_room_invitation FK_5f265075b215fc390a57523b12a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room_invitation
    ADD CONSTRAINT "FK_5f265075b215fc390a57523b12a" FOREIGN KEY ("roomId") REFERENCES public.chat_room(id) ON DELETE CASCADE;


--
-- Name: announcement_read FK_603a7b1e7aa0533c6c88e9bfafe; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.announcement_read
    ADD CONSTRAINT "FK_603a7b1e7aa0533c6c88e9bfafe" FOREIGN KEY ("announcementId") REFERENCES public.announcement(id) ON DELETE CASCADE;


--
-- Name: flash_like FK_60c4af1c19a7a75f1592f93b287; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flash_like
    ADD CONSTRAINT "FK_60c4af1c19a7a75f1592f93b287" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: antenna FK_6446c571a0e8d0f05f01c789096; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.antenna
    ADD CONSTRAINT "FK_6446c571a0e8d0f05f01c789096" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_memo FK_650b49c5639b5840ee6a2b8f83e; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_memo
    ADD CONSTRAINT "FK_650b49c5639b5840ee6a2b8f83e" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: following FK_6516c5a6f3c015b4eed39978be5; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.following
    ADD CONSTRAINT "FK_6516c5a6f3c015b4eed39978be5" FOREIGN KEY ("followerId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: reversi_game FK_6649a4e8c5d5cf32fb03b5da9f6; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reversi_game
    ADD CONSTRAINT "FK_6649a4e8c5d5cf32fb03b5da9f6" FOREIGN KEY ("user2Id") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_memo FK_66ac4a82894297fd09ba61f3d35; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_memo
    ADD CONSTRAINT "FK_66ac4a82894297fd09ba61f3d35" FOREIGN KEY ("targetUserId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: poll_vote FK_66d2bd2ee31d14bcc23069a89f8; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.poll_vote
    ADD CONSTRAINT "FK_66d2bd2ee31d14bcc23069a89f8" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_group_joining FK_67dc758bc0566985d1b3d399865; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_joining
    ADD CONSTRAINT "FK_67dc758bc0566985d1b3d399865" FOREIGN KEY ("userGroupId") REFERENCES public.user_group(id) ON DELETE CASCADE;


--
-- Name: user_note_pining FK_68881008f7c3588ad7ecae471cf; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_note_pining
    ADD CONSTRAINT "FK_68881008f7c3588ad7ecae471cf" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: flash_like FK_6c16fe0e93b7a1951eca624b76a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flash_like
    ADD CONSTRAINT "FK_6c16fe0e93b7a1951eca624b76a" FOREIGN KEY ("flashId") REFERENCES public.flash(id) ON DELETE CASCADE;


--
-- Name: channel_following FK_6d8084ec9496e7334a4602707e1; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_following
    ADD CONSTRAINT "FK_6d8084ec9496e7334a4602707e1" FOREIGN KEY ("followerId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_profile FK_6dc44f1ceb65b1e72bacef2ca27; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_profile
    ADD CONSTRAINT "FK_6dc44f1ceb65b1e72bacef2ca27" FOREIGN KEY ("pinnedPageId") REFERENCES public.page(id) ON DELETE SET NULL;


--
-- Name: antenna FK_709d7d32053d0dd7620f678eeb9; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.antenna
    ADD CONSTRAINT "FK_709d7d32053d0dd7620f678eeb9" FOREIGN KEY ("userListId") REFERENCES public.user_list(id) ON DELETE CASCADE;


--
-- Name: bubble_game_record FK_75276757070d21fdfaf4c052909; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bubble_game_record
    ADD CONSTRAINT "FK_75276757070d21fdfaf4c052909" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: chat_message FK_79a26e7a4d9afa5e4fc05f134ed; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message
    ADD CONSTRAINT "FK_79a26e7a4d9afa5e4fc05f134ed" FOREIGN KEY ("fromUserId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: renote_muting FK_7aa72a5fe76019bfe8e5e0e8b7d; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.renote_muting
    ADD CONSTRAINT "FK_7aa72a5fe76019bfe8e5e0e8b7d" FOREIGN KEY ("muterId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: renote_muting FK_7eac97594bcac5ffcf2068089b6; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.renote_muting
    ADD CONSTRAINT "FK_7eac97594bcac5ffcf2068089b6" FOREIGN KEY ("muteeId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: channel_note_pining FK_8125f950afd3093acb10d2db8a8; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_note_pining
    ADD CONSTRAINT "FK_8125f950afd3093acb10d2db8a8" FOREIGN KEY ("channelId") REFERENCES public.channel(id) ON DELETE CASCADE;


--
-- Name: channel FK_823bae55bd81b3be6e05cff4383; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel
    ADD CONSTRAINT "FK_823bae55bd81b3be6e05cff4383" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: announcement_read FK_8288151386172b8109f7239ab28; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.announcement_read
    ADD CONSTRAINT "FK_8288151386172b8109f7239ab28" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: channel_favorite FK_8302bd27226605ece14842fb25a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_favorite
    ADD CONSTRAINT "FK_8302bd27226605ece14842fb25a" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: chat_room_invitation FK_8552bb38e7ed038c5bdd398a384; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room_invitation
    ADD CONSTRAINT "FK_8552bb38e7ed038c5bdd398a384" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: drive_file FK_860fa6f6c7df5bb887249fba22e; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.drive_file
    ADD CONSTRAINT "FK_860fa6f6c7df5bb887249fba22e" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: gallery_like FK_8fd5215095473061855ceb948cf; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gallery_like
    ADD CONSTRAINT "FK_8fd5215095473061855ceb948cf" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: muting FK_93060675b4a79a577f31d260c67; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.muting
    ADD CONSTRAINT "FK_93060675b4a79a577f31d260c67" FOREIGN KEY ("muterId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: promo_read FK_9657d55550c3d37bfafaf7d4b05; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.promo_read
    ADD CONSTRAINT "FK_9657d55550c3d37bfafaf7d4b05" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: sw_subscription FK_97754ca6f2baff9b4abb7f853dd; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sw_subscription
    ADD CONSTRAINT "FK_97754ca6f2baff9b4abb7f853dd" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: gallery_post FK_985b836dddd8615e432d7043ddb; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gallery_post
    ADD CONSTRAINT "FK_985b836dddd8615e432d7043ddb" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: access_token FK_9949557d0e1b2c19e5344c171e9; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_token
    ADD CONSTRAINT "FK_9949557d0e1b2c19e5344c171e9" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: channel FK_999da2bcc7efadbfe0e92d3bc19; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel
    ADD CONSTRAINT "FK_999da2bcc7efadbfe0e92d3bc19" FOREIGN KEY ("bannerId") REFERENCES public.drive_file(id) ON DELETE SET NULL;


--
-- Name: flash FK_9b88250fc2fd009b8f1b5623ed5; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.flash
    ADD CONSTRAINT "FK_9b88250fc2fd009b8f1b5623ed5" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: clip_note FK_a012eaf5c87c65da1deb5fdbfa3; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip_note
    ADD CONSTRAINT "FK_a012eaf5c87c65da1deb5fdbfa3" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: moderation_log FK_a08ad074601d204e0f69da9a954; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.moderation_log
    ADD CONSTRAINT "FK_a08ad074601d204e0f69da9a954" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: access_token FK_a3ff16c90cc87a82a0b5959e560; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_token
    ADD CONSTRAINT "FK_a3ff16c90cc87a82a0b5959e560" FOREIGN KEY ("appId") REFERENCES public.app(id) ON DELETE CASCADE;


--
-- Name: promo_read FK_a46a1a603ecee695d7db26da5f4; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.promo_read
    ADD CONSTRAINT "FK_a46a1a603ecee695d7db26da5f4" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: follow_request FK_a7fd92dd6dc519e6fb435dd108f; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.follow_request
    ADD CONSTRAINT "FK_a7fd92dd6dc519e6fb435dd108f" FOREIGN KEY ("followerId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: abuse_user_report FK_a9021cc2e1feb5f72d3db6e9f5f; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_user_report
    ADD CONSTRAINT "FK_a9021cc2e1feb5f72d3db6e9f5f" FOREIGN KEY ("targetUserId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: page FK_a9ca79ad939bf06066b81c9d3aa; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.page
    ADD CONSTRAINT "FK_a9ca79ad939bf06066b81c9d3aa" FOREIGN KEY ("eyeCatchingImageId") REFERENCES public.drive_file(id) ON DELETE SET NULL;


--
-- Name: abuse_report_notification_recipient FK_abuse_report_notification_recipient_systemWebhookId; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_report_notification_recipient
    ADD CONSTRAINT "FK_abuse_report_notification_recipient_systemWebhookId" FOREIGN KEY ("systemWebhookId") REFERENCES public.system_webhook(id) ON DELETE CASCADE;


--
-- Name: abuse_report_notification_recipient FK_abuse_report_notification_recipient_userId1; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_report_notification_recipient
    ADD CONSTRAINT "FK_abuse_report_notification_recipient_userId1" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: abuse_report_notification_recipient FK_abuse_report_notification_recipient_userId2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.abuse_report_notification_recipient
    ADD CONSTRAINT "FK_abuse_report_notification_recipient_userId2" FOREIGN KEY ("userId") REFERENCES public.user_profile("userId") ON DELETE CASCADE;


--
-- Name: page FK_ae1d917992dd0c9d9bbdad06c4a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.page
    ADD CONSTRAINT "FK_ae1d917992dd0c9d9bbdad06c4a" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: poll_vote FK_aecfbd5ef60374918e63ee95fa7; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.poll_vote
    ADD CONSTRAINT "FK_aecfbd5ef60374918e63ee95fa7" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: user FK_afc64b53f8db3707ceb34eb28e2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public."user"
    ADD CONSTRAINT "FK_afc64b53f8db3707ceb34eb28e2" FOREIGN KEY ("bannerId") REFERENCES public.drive_file(id) ON DELETE SET NULL;


--
-- Name: note_watching FK_b0134ec406e8d09a540f8182888; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_watching
    ADD CONSTRAINT "FK_b0134ec406e8d09a540f8182888" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: gallery_like FK_b1cb568bfe569e47b7051699fc8; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.gallery_like
    ADD CONSTRAINT "FK_b1cb568bfe569e47b7051699fc8" FOREIGN KEY ("postId") REFERENCES public.gallery_post(id) ON DELETE CASCADE;


--
-- Name: chat_approval FK_b1d46037f23d170da5c05fdf755; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_approval
    ADD CONSTRAINT "FK_b1d46037f23d170da5c05fdf755" FOREIGN KEY ("otherId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: registration_ticket FK_b6f93f2f30bdbb9a5ebdc7c7189; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.registration_ticket
    ADD CONSTRAINT "FK_b6f93f2f30bdbb9a5ebdc7c7189" FOREIGN KEY ("usedById") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_list FK_b7fcefbdd1c18dce86687531f99; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list
    ADD CONSTRAINT "FK_b7fcefbdd1c18dce86687531f99" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: drive_file FK_bb90d1956dafc4068c28aa7560a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.drive_file
    ADD CONSTRAINT "FK_bb90d1956dafc4068c28aa7560a" FOREIGN KEY ("folderId") REFERENCES public.drive_folder(id) ON DELETE SET NULL;


--
-- Name: registration_ticket FK_beba993576db0261a15364ea96e; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.registration_ticket
    ADD CONSTRAINT "FK_beba993576db0261a15364ea96e" FOREIGN KEY ("createdById") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_group_invitation FK_bfbc6305547539369fe73eb144a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_invitation
    ADD CONSTRAINT "FK_bfbc6305547539369fe73eb144a" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_note_pining FK_bfbc6f79ba4007b4ce5097f08d6; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_note_pining
    ADD CONSTRAINT "FK_bfbc6f79ba4007b4ce5097f08d6" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: auth_session FK_c072b729d71697f959bde66ade0; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.auth_session
    ADD CONSTRAINT "FK_c072b729d71697f959bde66ade0" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: chat_room_membership FK_c25143ebab714e930aeca1c0e8d; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room_membership
    ADD CONSTRAINT "FK_c25143ebab714e930aeca1c0e8d" FOREIGN KEY ("roomId") REFERENCES public.chat_room(id) ON DELETE CASCADE;


--
-- Name: meta FK_c80e4079d632f95eac06a9d28cc; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.meta
    ADD CONSTRAINT "FK_c80e4079d632f95eac06a9d28cc" FOREIGN KEY ("rootUserId") REFERENCES public."user"(id) ON DELETE SET NULL;


--
-- Name: messaging_message FK_cac14a4e3944454a5ce7daa5142; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.messaging_message
    ADD CONSTRAINT "FK_cac14a4e3944454a5ce7daa5142" FOREIGN KEY ("recipientId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_list_membership FK_cddcaf418dc4d392ecfcca842a7; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_list_membership
    ADD CONSTRAINT "FK_cddcaf418dc4d392ecfcca842a7" FOREIGN KEY ("userListId") REFERENCES public.user_list(id) ON DELETE CASCADE;


--
-- Name: page_like FK_cf8782626dced3176038176a847; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.page_like
    ADD CONSTRAINT "FK_cf8782626dced3176038176a847" FOREIGN KEY ("pageId") REFERENCES public.page(id) ON DELETE CASCADE;


--
-- Name: channel_favorite FK_d3ca0db011b75ac2a940a2337d2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.channel_favorite
    ADD CONSTRAINT "FK_d3ca0db011b75ac2a940a2337d2" FOREIGN KEY ("channelId") REFERENCES public.channel(id) ON DELETE CASCADE;


--
-- Name: chat_room_membership FK_d99c5279460fb77ef58c596ce51; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room_membership
    ADD CONSTRAINT "FK_d99c5279460fb77ef58c596ce51" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: poll FK_da851e06d0dfe2ef397d8b1bf1b; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.poll
    ADD CONSTRAINT "FK_da851e06d0dfe2ef397d8b1bf1b" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: role_assignment FK_db5b72c16227c97ca88734d5c2b; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role_assignment
    ADD CONSTRAINT "FK_db5b72c16227c97ca88734d5c2b" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: auth_session FK_dbe037d4bddd17b03a1dc778dee; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.auth_session
    ADD CONSTRAINT "FK_dbe037d4bddd17b03a1dc778dee" FOREIGN KEY ("appId") REFERENCES public.app(id) ON DELETE CASCADE;


--
-- Name: user_group_invite FK_e10924607d058004304611a436a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_invite
    ADD CONSTRAINT "FK_e10924607d058004304611a436a" FOREIGN KEY ("userGroupId") REFERENCES public.user_group(id) ON DELETE CASCADE;


--
-- Name: reversi_matching FK_e247b23a3c9b45f89ec1299d066; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reversi_matching
    ADD CONSTRAINT "FK_e247b23a3c9b45f89ec1299d066" FOREIGN KEY ("childId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: promo_note FK_e263909ca4fe5d57f8d4230dd5c; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.promo_note
    ADD CONSTRAINT "FK_e263909ca4fe5d57f8d4230dd5c" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: note_draft FK_e4983f28b4b18b03491536052f5; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_draft
    ADD CONSTRAINT "FK_e4983f28b4b18b03491536052f5" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: note_unread FK_e637cba4dc4410218c4251260e4; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note_unread
    ADD CONSTRAINT "FK_e637cba4dc4410218c4251260e4" FOREIGN KEY ("noteId") REFERENCES public.note(id) ON DELETE CASCADE;


--
-- Name: clip_note FK_ebe99317bbbe9968a0c6f579adf; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip_note
    ADD CONSTRAINT "FK_ebe99317bbbe9968a0c6f579adf" FOREIGN KEY ("clipId") REFERENCES public.clip(id) ON DELETE CASCADE;


--
-- Name: muting FK_ec96b4fed9dae517e0dbbe0675c; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.muting
    ADD CONSTRAINT "FK_ec96b4fed9dae517e0dbbe0675c" FOREIGN KEY ("muteeId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: chat_message FK_f006b8a76efd1abf9f221c175ce; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message
    ADD CONSTRAINT "FK_f006b8a76efd1abf9f221c175ce" FOREIGN KEY ("toRoomId") REFERENCES public.chat_room(id) ON DELETE CASCADE;


--
-- Name: chat_room FK_f0d8ad64243fa2ca2800da0dfd6; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_room
    ADD CONSTRAINT "FK_f0d8ad64243fa2ca2800da0dfd6" FOREIGN KEY ("ownerId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: role_assignment FK_f0de67fd09cd3cd0aabca79994d; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.role_assignment
    ADD CONSTRAINT "FK_f0de67fd09cd3cd0aabca79994d" FOREIGN KEY ("roleId") REFERENCES public.role(id) ON DELETE CASCADE;


--
-- Name: note FK_f22169eb10657bded6d875ac8f9; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.note
    ADD CONSTRAINT "FK_f22169eb10657bded6d875ac8f9" FOREIGN KEY ("channelId") REFERENCES public.channel(id) ON DELETE CASCADE;


--
-- Name: webhook FK_f272c8c8805969e6a6449c77b3c; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook
    ADD CONSTRAINT "FK_f272c8c8805969e6a6449c77b3c" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_group_joining FK_f3a1b4bd0c7cabba958a0c0b231; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_group_joining
    ADD CONSTRAINT "FK_f3a1b4bd0c7cabba958a0c0b231" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_keypair FK_f4853eb41ab722fe05f81cedeb6; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_keypair
    ADD CONSTRAINT "FK_f4853eb41ab722fe05f81cedeb6" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: drive_folder FK_f4fc06e49c0171c85f1c48060d2; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.drive_folder
    ADD CONSTRAINT "FK_f4fc06e49c0171c85f1c48060d2" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: reversi_game FK_f7467510c60a45ce5aca6292743; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reversi_game
    ADD CONSTRAINT "FK_f7467510c60a45ce5aca6292743" FOREIGN KEY ("user1Id") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: registry_item FK_fb9d21ba0abb83223263df6bcb3; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.registry_item
    ADD CONSTRAINT "FK_fb9d21ba0abb83223263df6bcb3" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: clip_favorite FK_fce61c7986cee54393e79f1d849; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.clip_favorite
    ADD CONSTRAINT "FK_fce61c7986cee54393e79f1d849" FOREIGN KEY ("clipId") REFERENCES public.clip(id) ON DELETE CASCADE;


--
-- Name: chat_message FK_fd0f9a4879430239715ad4f8e2a; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.chat_message
    ADD CONSTRAINT "FK_fd0f9a4879430239715ad4f8e2a" FOREIGN KEY ("fileId") REFERENCES public.drive_file(id) ON DELETE SET NULL;


--
-- Name: announcement FK_fd25dfe3da37df1715f11ba6ec8; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.announcement
    ADD CONSTRAINT "FK_fd25dfe3da37df1715f11ba6ec8" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- Name: user_security_key FK_ff9ca3b5f3ee3d0681367a9b447; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_security_key
    ADD CONSTRAINT "FK_ff9ca3b5f3ee3d0681367a9b447" FOREIGN KEY ("userId") REFERENCES public."user"(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--


