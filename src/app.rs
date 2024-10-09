use std::fs;
use std::path::{Path, PathBuf};

use rand::rngs::OsRng;
use rand::RngCore;

use crate::paths::Paths;

pub mod auth;
pub mod config;
pub mod ddns;
pub mod formats;
pub mod index;
pub mod ndb;
pub mod peaks;
pub mod playlist;
pub mod scanner;
pub mod thumbnail;

#[cfg(test)]
pub mod test;

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error(transparent)]
	ThreadPoolBuilder(#[from] rayon::ThreadPoolBuildError),
	#[error(transparent)]
	ThreadJoining(#[from] tokio::task::JoinError),

	#[error("Filesystem error for `{0}`: `{1}`")]
	Io(PathBuf, std::io::Error),
	#[error(transparent)]
	Ape(#[from] ape::Error),
	#[error("ID3 error in `{0}`: `{1}`")]
	Id3(PathBuf, id3::Error),
	#[error("Metaflac error in `{0}`: `{1}`")]
	Metaflac(PathBuf, metaflac::Error),
	#[error("Mp4aMeta error in `{0}`: `{1}`")]
	Mp4aMeta(PathBuf, mp4ameta::Error),
	#[error(transparent)]
	Opus(#[from] opus_headers::ParseError),
	#[error(transparent)]
	Vorbis(#[from] lewton::VorbisError),
	#[error("Could not find a Vorbis comment within flac file")]
	VorbisCommentNotFoundInFlacFile,
	#[error("Could not read thumbnail image in `{0}`:\n\n{1}")]
	Image(PathBuf, image::error::ImageError),
	#[error("This file format is not supported: {0}")]
	UnsupportedFormat(&'static str),

	#[error("No tracks found in audio file: {0}")]
	MediaEmpty(PathBuf),
	#[error(transparent)]
	MediaDecodeError(symphonia::core::errors::Error),
	#[error(transparent)]
	MediaDecoderError(symphonia::core::errors::Error),
	#[error(transparent)]
	MediaPacketError(symphonia::core::errors::Error),
	#[error(transparent)]
	MediaProbeError(symphonia::core::errors::Error),

	#[error(transparent)]
	PeaksSerialization(bitcode::Error),
	#[error(transparent)]
	PeaksDeserialization(bitcode::Error),

	#[error(transparent)]
	NativeDatabase(#[from] native_db::db_type::Error),
	#[error("Could not initialize database")]
	NativeDatabaseCreationError(native_db::db_type::Error),

	#[error("DDNS update query failed with HTTP status code `{0}`")]
	UpdateQueryFailed(u16),
	#[error("DDNS update query failed due to a transport error")]
	UpdateQueryTransport,

	#[error("Auth secret does not have the expected format")]
	AuthenticationSecretInvalid,
	#[error("Missing auth secret")]
	AuthenticationSecretNotFound,
	#[error("Missing settings")]
	MiscSettingsNotFound,
	#[error("Index album art pattern is not a valid regex")]
	IndexAlbumArtPatternInvalid,
	#[error("DDNS update URL is invalid")]
	DDNSUpdateURLInvalid,

	#[error(transparent)]
	Toml(#[from] toml::de::Error),
	#[error("Could not deserialize collection")]
	IndexDeserializationError,
	#[error("Could not serialize collection")]
	IndexSerializationError,

	#[error("The following virtual path could not be mapped to a real path: `{0}`")]
	CouldNotMapToRealPath(PathBuf),
	#[error("User not found")]
	UserNotFound,
	#[error("Directory not found: {0}")]
	DirectoryNotFound(PathBuf),
	#[error("Artist not found")]
	ArtistNotFound,
	#[error("Album not found")]
	AlbumNotFound,
	#[error("Genre not found")]
	GenreNotFound,
	#[error("Song not found")]
	SongNotFound,
	#[error("Invalid search query syntax")]
	SearchQueryParseError,
	#[error("Playlist not found")]
	PlaylistNotFound,
	#[error("No embedded artwork was found in `{0}`")]
	EmbeddedArtworkNotFound(PathBuf),

	#[error("Cannot use empty username")]
	EmptyUsername,
	#[error("Cannot use empty password")]
	EmptyPassword,
	#[error("Username already exists")]
	DuplicateUsername,
	#[error("Username does not exist")]
	IncorrectUsername,
	#[error("Password does not match username")]
	IncorrectPassword,
	#[error("Invalid auth token")]
	InvalidAuthToken,
	#[error("Incorrect authorization scope")]
	IncorrectAuthorizationScope,
	#[error("Failed to hash password")]
	PasswordHashing,
	#[error("Failed to encode authorization token")]
	AuthorizationTokenEncoding,
	#[error("Failed to encode Branca token")]
	BrancaTokenEncoding,
}

#[derive(Clone)]
pub struct App {
	pub port: u16,
	pub web_dir_path: PathBuf,
	pub swagger_dir_path: PathBuf,
	pub ddns_manager: ddns::Manager,
	pub scanner: scanner::Scanner,
	pub index_manager: index::Manager,
	pub config_manager: config::Manager,
	pub peaks_manager: peaks::Manager,
	pub playlist_manager: playlist::Manager,
	pub thumbnail_manager: thumbnail::Manager,
}

impl App {
	pub async fn new(port: u16, paths: Paths) -> Result<Self, Error> {
		fs::create_dir_all(&paths.data_dir_path)
			.map_err(|e| Error::Io(paths.data_dir_path.clone(), e))?;

		fs::create_dir_all(&paths.web_dir_path)
			.map_err(|e| Error::Io(paths.web_dir_path.clone(), e))?;

		fs::create_dir_all(&paths.swagger_dir_path)
			.map_err(|e| Error::Io(paths.swagger_dir_path.clone(), e))?;

		let peaks_dir_path = paths.cache_dir_path.join("peaks");
		fs::create_dir_all(&peaks_dir_path).map_err(|e| Error::Io(peaks_dir_path.clone(), e))?;

		let thumbnails_dir_path = paths.cache_dir_path.join("thumbnails");
		fs::create_dir_all(&thumbnails_dir_path)
			.map_err(|e| Error::Io(thumbnails_dir_path.clone(), e))?;

		let auth_secret_file_path = paths.data_dir_path.join("auth.secret");
		let auth_secret = Self::get_or_create_auth_secret(&auth_secret_file_path).await?;

		let config_manager = config::Manager::new(&paths.config_file_path, auth_secret).await?;
		let ddns_manager = ddns::Manager::new(config_manager.clone());
		let ndb_manager = ndb::Manager::new(&paths.data_dir_path)?;
		let index_manager = index::Manager::new(&paths.data_dir_path).await?;
		let scanner = scanner::Scanner::new(index_manager.clone(), config_manager.clone()).await?;
		let peaks_manager = peaks::Manager::new(peaks_dir_path);
		let playlist_manager = playlist::Manager::new(ndb_manager);
		let thumbnail_manager = thumbnail::Manager::new(thumbnails_dir_path);

		Ok(Self {
			port,
			web_dir_path: paths.web_dir_path,
			swagger_dir_path: paths.swagger_dir_path,
			ddns_manager,
			scanner,
			index_manager,
			config_manager,
			peaks_manager,
			playlist_manager,
			thumbnail_manager,
		})
	}

	async fn get_or_create_auth_secret(path: &Path) -> Result<auth::Secret, Error> {
		match tokio::fs::read(&path).await {
			Ok(s) => Ok(auth::Secret {
				key: s
					.try_into()
					.map_err(|_| Error::AuthenticationSecretInvalid)?,
			}),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				let mut secret = auth::Secret::default();
				OsRng.fill_bytes(&mut secret.key);
				tokio::fs::write(&path, &secret.key)
					.await
					.map_err(|_| Error::AuthenticationSecretInvalid)?;
				Ok(secret)
			}
			Err(e) => return Err(Error::Io(path.to_owned(), e)),
		}
	}
}
