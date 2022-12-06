use derive_more::Display;
use git::bstr::BString;
use git::protocol::transport::client::ReadlineBufRead;
use git::protocol::transport::packetline;
use git_repository as git;
use nom::branch::alt;
use nom::bytes::complete::{tag, take_while1};
use nom::character::complete::char;
use nom::combinator::{eof, opt};
use nom::error::context;
use nom::IResult;
use std::cell::Cell;

pub type ReportStatusV2 = (UnpackResult, Vec<CommandStatusV2>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnpackResult {
    Ok,
    ErrorMsg(ErrorMsg),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandStatusV2 {
    Ok(RefName, Vec<OptionLine>),
    Fail(RefName, ErrorMsg),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandStatusV2Line {
    Ok(RefName),
    Fail(RefName, ErrorMsg),
    OptionLine(OptionLine),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionLine {
    OptionRefName(RefName),
    OptionOldOid(git::hash::ObjectId),
    OptionNewOid(git::hash::ObjectId),
    OptionForce,
}

#[derive(Clone, Debug, Display, Eq, PartialEq)]
pub struct ErrorMsg(BString);

#[derive(Clone, Debug, Display, Eq, PartialEq)]
pub struct RefName(BString);

pub async fn read_and_parse<'a, T>(reader: &'a mut T) -> Result<ReportStatusV2, ParseError>
where
    T: ReadlineBufRead + 'a,
{
    let unpack_result = read_data_line_and_parse_with::<_, nom::error::Error<_>>(
        reader,
        parse_unpack_status,
        ParseError::FailedToReadUnpackStatus,
    )
    .await?;

    let command_statuses_v2 =
        read_and_parse_command_statuses_v2::<nom::error::Error<_>>(reader).await?;

    Ok((unpack_result, command_statuses_v2))
}

fn parse_unpack_status<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], UnpackResult, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context("unpack-status", |input| {
        let (next_input, _unpack) = tag(b"unpack")(input)?;
        let (next_input, _space) = char(' ')(next_input)?;
        let (next_input, unpack_result) = parse_unpack_result(next_input)?;
        let (next_input, _newline) = opt(char('\n'))(next_input)?;
        let (next_input, _) = eof(next_input)?;
        Ok((next_input, unpack_result))
    })(input)
}

fn parse_unpack_result<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], UnpackResult, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context(
        "unpack-result",
        alt((
            nom::combinator::map(tag(b"ok"), |_| UnpackResult::Ok),
            nom::combinator::map(parse_error_msg, UnpackResult::ErrorMsg),
        )),
    )(input)
}

// TODO: send commit without tree to trigger error for test case
fn parse_error_msg<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], ErrorMsg, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context("error-msg", |input| {
        let (next_input, error_msg) =
            // The core rules for the ABNF standard define OCTET as %x00-FF.
            //
            // However, representing this accurately with `take_while1(|chr|
            // 0x00 <= chr && chr <= 0xFF)` exceeds the limits of the u8 type,
            // so we use `rest` instead.
            nom::combinator::verify(nom::combinator::rest, |bytes: &[u8]| {
                !bytes.is_empty() && bytes != b"ok"
            })(input)?;

        Ok((next_input, ErrorMsg(BString::from(error_msg))))
    })(input)
}

async fn read_and_parse_command_statuses_v2<'a, E>(
    reader: &'a mut (dyn ReadlineBufRead + 'a),
) -> Result<Vec<CommandStatusV2>, ParseError>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]> + std::fmt::Debug,
{
    let candidate: Cell<Option<CommandStatusV2>> = Cell::new(None);
    let mut command_statuses_v2: Vec<CommandStatusV2> = Vec::new();

    while let Some(outcome) = reader.readline().await {
        let line = as_slice(outcome)?;
        let command_status_v2_line = parse_with(parse_command_status_v2_line, line)?;

        match (candidate.take(), command_status_v2_line) {
            // No `command-ok` candidate for adding `option-line`s to, followed
            // by a `command-ok` status line. For well-behaved input, this is
            // either the first line or a the line after a `command-fail` line.
            //
            // Set the line as a candidate for adding `option-lines` to.
            (None, CommandStatusV2Line::Ok(ref_name)) => {
                candidate.set(Some(CommandStatusV2::Ok(ref_name, Vec::new())));
            }
            // No `command-ok` candidate for adding `option-line`s to, followed
            // by a `command-fail` status line. For well-behaved input, this is
            // either the first line or a the line after a `command-fail` line.
            //
            // Immediately promote the line to `command-status-v2` since
            // `option-line` doesn't apply to `command-fail`.
            (None, CommandStatusV2Line::Fail(ref_name, error_msg)) => {
                command_statuses_v2.push(CommandStatusV2::Fail(ref_name, error_msg));
            }
            // A `command-ok` status line followed by a `command-ok` status
            // line.
            //
            // Promote the previous candidate to `command-status-v2` and set the
            // current line as the new candidate.
            (Some(command_status_v2), CommandStatusV2Line::Ok(ref_name)) => {
                command_statuses_v2.push(command_status_v2.clone());
                let new_candidate = CommandStatusV2::Ok(ref_name, Vec::new());
                candidate.set(Some(new_candidate));
            }
            // A `command-ok` status line followed by a `command-fail` status line.
            //
            // Promote both the previous candidate and the current line to
            // `command-status-v2`, and reset the candidate since `option-line`
            // doesn't apply to `command-fail`.
            (Some(command_status_v2), CommandStatusV2Line::Fail(ref_name, error_msg)) => {
                command_statuses_v2.push(command_status_v2.clone());
                command_statuses_v2.push(CommandStatusV2::Fail(ref_name, error_msg));
                // This should be redundant because `std::cell::Cell::take()`
                // should leave `Default::default()`.
                candidate.set(None);
            }
            // No `command-ok` candidate for adding `option-line`s to, followed
            // by an `option-line`.
            //
            // This is invalid since we don't have a canidate `command-ok` line
            // to add `option-line`s to.
            (None, CommandStatusV2Line::OptionLine(_)) => {
                return Err(ParseError::UnexpectedOptionLine)
            }
            // A `command-ok` line followed by an `option-line`.
            //
            // Add the `option-line` to the `command-ok` and set it as the new
            // candidate in case the next line is also an `option-line`.
            (
                Some(CommandStatusV2::Ok(ref_name, mut option_lines)),
                CommandStatusV2Line::OptionLine(option_line),
            ) => {
                option_lines.push(option_line);
                let new_candidate = CommandStatusV2::Ok(ref_name, option_lines);
                candidate.set(Some(new_candidate));
            }
            // A `command-fail` line followed by an `option-line`.
            //
            // This is invalid since we don't have a canidate `command-ok` line
            // to add `option-line`s to.
            (Some(CommandStatusV2::Fail(_, _)), CommandStatusV2Line::OptionLine(_)) => {
                return Err(ParseError::UnexpectedOptionLine)
            }
        }
    }

    // The last line of the input produced a candidate which we need to
    // promote to a `command-status-v2`.
    match candidate.take() {
        // A `command-ok` line. This is the only valid candidate at this stage.
        //
        // Promote the candidate to `command-status-v2`.
        Some(CommandStatusV2::Ok(ref_name, option_lines)) => {
            command_statuses_v2.push(CommandStatusV2::Ok(ref_name, option_lines));
        }
        // A `command-fail` line. This is an invalid candidate.
        Some(CommandStatusV2::Fail(_, _)) => return Err(ParseError::UnexpectedCommandFailLine),
        None => (),
    }

    if command_statuses_v2.is_empty() {
        Err(ParseError::ExpectedOneOrMoreCommandStatusV2)
    } else {
        Ok(command_statuses_v2)
    }
}

fn parse_command_status_v2_line<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], CommandStatusV2Line, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context(
        "command-status-v2 line",
        alt((
            nom::combinator::map(parse_command_ok, CommandStatusV2Line::Ok),
            nom::combinator::map(parse_command_fail, |(ref_name, error_msg)| {
                CommandStatusV2Line::Fail(ref_name, error_msg)
            }),
            nom::combinator::map(parse_option_line, CommandStatusV2Line::OptionLine),
        )),
    )(input)
}

fn parse_command_ok<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], RefName, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context("command-ok", |input| {
        let (next_input, _unpack) = tag(b"ok")(input)?;
        let (next_input, _space) = char(' ')(next_input)?;
        let (next_input, refname) = parse_refname(next_input)?;
        let (next_input, _newline) = opt(char('\n'))(next_input)?;
        let (next_input, _) = eof(next_input)?;
        Ok((next_input, refname))
    })(input)
}

fn parse_command_fail<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], (RefName, ErrorMsg), E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context("command-fail", |input| {
        let (next_input, _unpack) = tag(b"ng")(input)?;
        let (next_input, _space) = char(' ')(next_input)?;
        let (next_input, refname) = parse_refname(next_input)?;
        let (next_input, _space) = char(' ')(next_input)?;
        let (next_input, error_msg) = parse_error_msg(next_input)?;
        let (next_input, _newline) = opt(char('\n'))(next_input)?;
        let (next_input, _) = eof(next_input)?;
        Ok((next_input, (refname, error_msg)))
    })(input)
}

// NOTE
// * This parser is intentionally overly-permissive for now since we treat
//   refnames as opaque values anyway.
// * `git_validate::refname` doesn't cover all of the validation cases
//    described in documentation.
fn parse_refname<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], RefName, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context("refname", |input| {
        let parser = nom::combinator::verify(
            take_while1(|chr| {
                0o040 <= chr
                    && !vec![0o177, b' ', b'~', b'^', b':', b'?', b'*', b'['].contains(&chr)
            }),
            |refname: &[u8]| git_validate::refname(refname.into()).is_ok(),
        );
        nom::combinator::map(parser, |refname: &[u8]| {
            RefName(BString::new(refname.to_vec()))
        })(input)
    })(input)
}

fn parse_option_line<'a, E>(input: &'a [u8]) -> IResult<&'a [u8], OptionLine, E>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    context("option-line", |input| {
        // TODO
        todo!("option-line")
    })(input)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    FailedToReadUnpackStatus,
    Io(String),
    ExpectedOneOrMoreCommandStatusV2,
    Nom(String),
    PacketLineDecode(String),
    UnexpectedCommandFailLine,
    UnexpectedFlush,
    UnexpectedDelimiter,
    UnexpectedOptionLine,
    UnexpectedResponseEnd,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            Self::FailedToReadUnpackStatus => "failed to read unpack status".to_string(),
            Self::Io(err) => format!("IO error: {}", err),
            Self::ExpectedOneOrMoreCommandStatusV2 => "expected one or more command status v2".to_string(),
            Self::Nom(err) => format!("nom error: {}", err),
            Self::PacketLineDecode(err) => err.to_string(),
            Self::UnexpectedCommandFailLine => "unexpected command fail line".to_string(),
            Self::UnexpectedFlush => "unexpected flush packet".to_string(),
            Self::UnexpectedDelimiter => "unexpected delimiter".to_string(),
            Self::UnexpectedOptionLine => "unexpected option line".to_string(),
            Self::UnexpectedResponseEnd => "unexpected response end".to_string(),
        };
        write!(f, "{}", msg)
    }
}

impl std::error::Error for ParseError {}

async fn read_data_line_and_parse_with<'a, Ok, E>(
    input: &'a mut (dyn ReadlineBufRead + 'a),
    parser: impl FnMut(&'a [u8]) -> IResult<&'a [u8], Ok>,
    read_err: ParseError,
) -> Result<Ok, ParseError>
where
    E: nom::error::ParseError<&'a [u8]> + nom::error::ContextError<&'a [u8]>,
{
    let line = read_data_line(input, read_err).await?;
    parse_with(parser, line)
}

fn parse_with<'a, Ok>(
    mut parser: impl FnMut(&'a [u8]) -> IResult<&'a [u8], Ok>,
    input: &'a [u8],
) -> Result<Ok, ParseError> {
    parser(input)
        .map(|x| x.1)
        .map_err(|err| ParseError::Nom(err.to_string()))
}

async fn read_data_line<'a>(
    input: &'a mut (dyn ReadlineBufRead + 'a),
    err: ParseError,
) -> Result<&'a [u8], ParseError> {
    match input.readline().await {
        Some(line) => as_slice(line),
        None => Err(err),
    }
}

// Similar to `as_slice()` on `packetline::PacketLineRef`
fn as_slice(
    readline_outcome: std::io::Result<
        Result<packetline::PacketLineRef<'_>, packetline::decode::Error>,
    >,
) -> Result<&[u8], ParseError> {
    let packet_line_ref = readline_outcome
        .map_err(|err| ParseError::Io(err.to_string()))?
        .map_err(|err| ParseError::PacketLineDecode(err.to_string()))?;

    match packet_line_ref {
        packetline::PacketLineRef::Data(data) => Ok(data),
        packetline::PacketLineRef::Flush => Err(ParseError::UnexpectedFlush),
        packetline::PacketLineRef::Delimiter => Err(ParseError::UnexpectedDelimiter),
        packetline::PacketLineRef::ResponseEnd => Err(ParseError::UnexpectedResponseEnd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use core::pin::Pin;
    use git::bstr::{BStr, ByteSlice};

    struct Fixture<'a>(&'a [u8]);

    impl<'a> Fixture<'a> {
        fn project(self: Pin<&mut Self>) -> Pin<&mut &'a [u8]> {
            unsafe { Pin::new(&mut self.get_unchecked_mut().0) }
        }
    }

    impl<'a> git::protocol::futures_io::AsyncRead for Fixture<'a> {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.project().poll_read(cx, buf)
        }
    }

    impl<'a> git::protocol::futures_io::AsyncBufRead for Fixture<'a> {
        fn poll_fill_buf(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<&[u8]>> {
            self.project().poll_fill_buf(cx)
        }

        fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
            self.project().consume(amt)
        }
    }

    #[async_trait(?Send)]
    impl<'a> git::protocol::transport::client::ReadlineBufRead for Fixture<'a> {
        async fn readline(
            &mut self,
        ) -> Option<std::io::Result<Result<packetline::PacketLineRef<'_>, packetline::decode::Error>>>
        {
            let bytes: &BStr = self.0.into();
            let mut lines = bytes.lines();
            let res = lines.next()?;
            self.0 = lines.as_bytes();
            Some(Ok(Ok(packetline::PacketLineRef::Data(res))))
        }
    }

    #[tokio::test]
    async fn test_read_and_parse_ok_0_command_status_v2() {
        let mut input = vec!["unpack ok"]
            .join("\n")
            .into_bytes();
        let mut reader = Fixture(&mut input);
        let result = read_and_parse(&mut reader).await;
        assert_eq!(
            result,
            Err(ParseError::ExpectedOneOrMoreCommandStatusV2),
            "report-status-v2"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_ok_1_command_status_v2_ok() {
        let mut input = vec!["unpack ok", "ok refs/heads/main"]
            .join("\n")
            .into_bytes();
        let mut reader = Fixture(&mut input);
        let result = read_and_parse(&mut reader).await;
        assert_eq!(
            result,
            Ok((
                UnpackResult::Ok,
                vec![CommandStatusV2::Ok(
                    RefName(BString::new(b"refs/heads/main".to_vec())),
                    Vec::new(),
                ),]
            )),
            "report-status-v2"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_ok_1_command_status_v2_fail() {
        let mut input = vec!["unpack ok", "ng refs/heads/main some error message"]
            .join("\n")
            .into_bytes();
        let mut reader = Fixture(&mut input);
        let result = read_and_parse(&mut reader).await;
        assert_eq!(
            result,
            Ok((
                UnpackResult::Ok,
                vec![CommandStatusV2::Fail(
                    RefName(BString::new(b"refs/heads/main".to_vec())),
                    ErrorMsg(BString::new(b"some error message".to_vec()))
                ),]
            )),
            "report-status-v2"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_ok_2_command_statuses_v2_ok_fail() {
        let mut input = vec![
            "unpack ok",
            "ok refs/heads/debug",
            "ng refs/heads/main non-fast-forward",
        ]
        .join("\n")
        .into_bytes();
        let mut reader = Fixture(&mut input);
        let result = read_and_parse(&mut reader).await;
        assert_eq!(
            result,
            Ok((
                UnpackResult::Ok,
                vec![
                    CommandStatusV2::Ok(
                        RefName(BString::new(b"refs/heads/debug".to_vec())),
                        Vec::new(),
                    ),
                    CommandStatusV2::Fail(
                        RefName(BString::new(b"refs/heads/main".to_vec())),
                        ErrorMsg(BString::new(b"non-fast-forward".to_vec()))
                    ),
                ]
            )),
            "report-status-v2"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_ok_2_command_statuses_v2_fail_ok() {
        let mut input = vec![
            "unpack ok",
            "ng refs/heads/main non-fast-forward",
            "ok refs/heads/debug",
        ]
        .join("\n")
        .into_bytes();
        let mut reader = Fixture(&mut input);
        let result = read_and_parse(&mut reader).await;
        assert_eq!(
            result,
            Ok((
                UnpackResult::Ok,
                vec![
                    CommandStatusV2::Fail(
                        RefName(BString::new(b"refs/heads/main".to_vec())),
                        ErrorMsg(BString::new(b"non-fast-forward".to_vec()))
                    ),
                    CommandStatusV2::Ok(
                        RefName(BString::new(b"refs/heads/debug".to_vec())),
                        Vec::new(),
                    ),
                ]
            )),
            "report-status-v2"
        )
    }

    #[test]
    fn test_parse_unpack_status_ok() {
        let input = b"unpack ok";
        let result = parse_unpack_status::<nom::error::Error<_>>(input);
        assert_eq!(result.map(|x| x.1), Ok(UnpackResult::Ok), "ok")
    }

    #[test]
    fn test_parse_unpack_status_ok_newline() {
        let input = b"unpack ok\n";
        let result = parse_unpack_status::<nom::error::Error<_>>(input);
        assert_eq!(result.map(|x| x.1), Ok(UnpackResult::Ok), "ok")
    }

    #[test]
    fn test_parse_unpack_status_error_msg() {
        let input = b"unpack some error message";
        let result = parse_unpack_status::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok(UnpackResult::ErrorMsg(ErrorMsg(BString::new(
                b"some error message".to_vec()
            )))),
            "error msg"
        )
    }

    #[test]
    fn test_parse_unpack_status_error_msg_newline() {
        let input = b"unpack some error message\n";
        let result = parse_unpack_status::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok(UnpackResult::ErrorMsg(ErrorMsg(BString::new(
                b"some error message\n".to_vec()
            )))),
            "error msg"
        )
    }

    #[test]
    fn test_parse_unpack_result_ok() {
        let input = b"ok";
        let result = parse_unpack_result::<nom::error::Error<_>>(input);
        assert_eq!(result.map(|x| x.1), Ok(UnpackResult::Ok), "ok");
    }

    #[test]
    fn test_parse_unpack_result_error_msg() {
        let input = b"some error message";
        let result = parse_unpack_result::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok(UnpackResult::ErrorMsg(ErrorMsg(BString::new(
                input.to_vec()
            )))),
            "error msg"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_0_option_lines() {
        let input = b"ok refs/heads/main";
        let mut reader = Fixture(input);
        let result = read_and_parse_command_statuses_v2::<nom::error::Error<_>>(&mut reader).await;
        assert_eq!(
            result,
            Ok(vec![CommandStatusV2::Ok(
                RefName(BString::new(b"refs/heads/main".to_vec())),
                Vec::new(),
            )]),
            "command-status-v2"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_0_option_lines_newline() {
        let input = b"ok refs/heads/main\n";
        let mut reader = Fixture(input);
        let result = read_and_parse_command_statuses_v2::<nom::error::Error<_>>(&mut reader).await;
        assert_eq!(
            result,
            Ok(vec![CommandStatusV2::Ok(
                RefName(BString::new(b"refs/heads/main".to_vec())),
                Vec::new(),
            )]),
            "command-status-v2"
        )
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_1_option_lines() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_1_option_lines_newline() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_2_option_lines() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_2_option_lines_newline() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_3_option_lines() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_3_option_lines_newline() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_4_option_lines() {
        todo!()
    }

    #[ignore]
    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_ok_v2_4_option_lines_newline() {
        todo!()
    }

    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_fail() {
        let input = b"ng refs/heads/main some error message";
        let mut reader = Fixture(input);
        let result = read_and_parse_command_statuses_v2::<nom::error::Error<_>>(&mut reader).await;
        assert_eq!(
            result,
            Ok(vec![CommandStatusV2::Fail(
                RefName(BString::new(b"refs/heads/main".to_vec())),
                ErrorMsg(BString::new(b"some error message".to_vec())),
            )]),
            "command-status-v2"
        )
    }

    #[tokio::test]
    async fn test_read_and_parse_command_status_v2_command_fail_newline() {
        let input = b"ng refs/heads/main some error message\n";
        let mut reader = Fixture(input);
        let result = read_and_parse_command_statuses_v2::<nom::error::Error<_>>(&mut reader).await;
        assert_eq!(
            result,
            Ok(vec![CommandStatusV2::Fail(
                RefName(BString::new(b"refs/heads/main".to_vec())),
                ErrorMsg(BString::new(b"some error message".to_vec())),
            )]),
            "command-status-v2"
        )
    }

    #[test]
    fn test_parse_command_ok() {
        let input = b"ok refs/heads/main";
        let result = parse_command_ok::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok(RefName(BString::new(b"refs/heads/main".to_vec()))),
            "command-ok"
        )
    }

    #[test]
    fn test_parse_command_ok_newline() {
        let input = b"ok refs/heads/main\n";
        let result = parse_command_ok::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok(RefName(BString::new(b"refs/heads/main".to_vec()))),
            "command-ok"
        )
    }

    #[test]
    fn test_parse_command_fail() {
        let input = b"ng refs/heads/main some error message";
        let result = parse_command_fail::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok((
                RefName(BString::new(b"refs/heads/main".to_vec())),
                ErrorMsg(BString::new(b"some error message".to_vec())),
            )),
            "command-fail"
        )
    }

    #[test]
    fn test_parse_command_fail_newline() {
        let input = b"ng refs/heads/main some error message\n";
        let result = parse_command_fail::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok((
                RefName(BString::new(b"refs/heads/main".to_vec())),
                ErrorMsg(BString::new(b"some error message\n".to_vec())),
            )),
            "command-fail"
        )
    }

    #[test]
    fn test_parse_error_msg_not_ok() {
        let input = b"some error message";
        let result = parse_error_msg::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Ok(ErrorMsg(BString::new(input.to_vec()))),
            "error msg not ok"
        )
    }

    #[test]
    fn test_parse_error_msg_ok() {
        let input = b"ok";
        let result = parse_error_msg::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Err(nom::Err::Error(nom::error::Error {
                input: input.as_bytes(),
                code: nom::error::ErrorKind::Verify
            })),
            "error msg is ok"
        )
    }

    #[test]
    fn test_parse_error_msg_empty() {
        let input = b"";
        let result = parse_error_msg::<nom::error::Error<_>>(input);
        assert_eq!(
            result.map(|x| x.1),
            Err(nom::Err::Error(nom::error::Error {
                input: input.as_bytes(),
                code: nom::error::ErrorKind::Verify
            })),
            "error msg is empty"
        )
    }
}